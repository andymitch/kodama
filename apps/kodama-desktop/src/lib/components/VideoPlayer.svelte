<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import type { VideoInitEvent, VideoSegmentEvent } from '$lib/types';

  let { sourceId }: { sourceId: string } = $props();

  let videoEl: HTMLVideoElement;
  let mediaSource: MediaSource | null = null;
  let sourceBuffer: SourceBuffer | null = null;
  let objectUrl: string = '';
  let queue: ArrayBuffer[] = [];
  const MAX_QUEUE_SIZE = 30; // Limit queue depth to prevent overflow
  let initialized = false;
  let initInProgress = false;
  let playStarted = false;
  let mediaSegmentsAppended = 0;
  let appendErrorCount = 0;
  let droppedSegments = 0;
  let unlistenInit: (() => void) | null = null;
  let unlistenSegment: (() => void) | null = null;

  function base64ToArrayBuffer(base64: string): ArrayBuffer {
    const binary = atob(base64);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) {
      bytes[i] = binary.charCodeAt(i);
    }
    return bytes.buffer;
  }

  function tryPlay() {
    if (playStarted || !videoEl) return;
    playStarted = true;
    console.log('[VideoPlayer] Calling play() now, buffered:', getBufferedInfo());
    videoEl.play().catch((e) => {
      console.warn('[VideoPlayer] play() rejected:', e);
      playStarted = false; // Allow retry
    });
  }

  function getBufferedInfo(): string {
    if (!sourceBuffer || sourceBuffer.buffered.length === 0) return 'none';
    const ranges = [];
    for (let i = 0; i < sourceBuffer.buffered.length; i++) {
      ranges.push(`${sourceBuffer.buffered.start(i).toFixed(3)}-${sourceBuffer.buffered.end(i).toFixed(3)}`);
    }
    return ranges.join(', ');
  }

  function appendBuffer(data: ArrayBuffer) {
    if (!sourceBuffer) return;

    if (sourceBuffer.updating) {
      // Drop oldest segments if queue is too large (prevent overflow)
      if (queue.length >= MAX_QUEUE_SIZE) {
        queue.shift(); // Drop oldest
        droppedSegments++;
        if (droppedSegments % 100 === 0) {
          console.warn('[VideoPlayer] Queue overflow, total dropped:', droppedSegments);
        }
      }
      queue.push(data);
      return;
    }

    try {
      sourceBuffer.appendBuffer(data);
    } catch (e) {
      appendErrorCount++;
      if (appendErrorCount <= 3) {
        console.error('[VideoPlayer] Failed to append buffer:', e);
      }
    }
  }

  function onUpdateEnd() {
    if (!sourceBuffer) return;

    mediaSegmentsAppended++;

    // Try to start playback once we have some buffered data
    if (!playStarted && sourceBuffer.buffered.length > 0) {
      const bufferedDuration = sourceBuffer.buffered.end(0) - sourceBuffer.buffered.start(0);
      if (bufferedDuration > 0.05) {
        tryPlay();
      }
    }

    // Auto-resume if paused but buffer is available (low latency - only need ~2 frames ahead)
    if (videoEl && videoEl.paused && playStarted && sourceBuffer.buffered.length > 0) {
      const currentTime = videoEl.currentTime;
      const bufferedEnd = sourceBuffer.buffered.end(sourceBuffer.buffered.length - 1);
      if (bufferedEnd - currentTime > 0.1) {  // ~3 frames at 30fps, very low latency
        console.log('[VideoPlayer] Auto-resuming paused video, buffer ahead:', (bufferedEnd - currentTime).toFixed(2), 's');
        videoEl.play().catch(err => console.warn('[VideoPlayer] Auto-resume failed:', err));
      }
    }

    // Trim buffer: keep only data ahead of current playback position
    // Remove anything more than 5 seconds behind currentTime
    if (videoEl && sourceBuffer.buffered.length > 0) {
      const currentTime = videoEl.currentTime;
      const start = sourceBuffer.buffered.start(0);
      const end = sourceBuffer.buffered.end(sourceBuffer.buffered.length - 1);

      // Remove old data that we've already played
      if (currentTime - start > 5) {
        const removeEnd = Math.min(currentTime - 1, end - 0.1); // Keep at least 1 sec behind, don't remove too close to end
        if (removeEnd > start) {
          try {
            sourceBuffer.remove(start, removeEnd);
            return; // wait for remove to complete before processing queue
          } catch (e) {
            console.error('[VideoPlayer] Failed to remove buffer:', e);
          }
        }
      }
    }

    // Process queued segments
    if (queue.length > 0 && !sourceBuffer.updating) {
      const next = queue.shift()!;
      try {
        sourceBuffer.appendBuffer(next);
      } catch (e) {
        appendErrorCount++;
        if (appendErrorCount <= 3) {
          console.error('Failed to append queued buffer (#' + appendErrorCount + '):', e);
        }
      }
    }
  }

  onMount(async () => {
    if (!('__TAURI__' in window)) return;

    const { listen } = await import('@tauri-apps/api/event');

    console.log('[VideoPlayer] Listening for source:', sourceId);

    unlistenInit = await listen<VideoInitEvent>('video-init', (event) => {
      console.log('[VideoPlayer] Initializing:', event.payload.width, 'x', event.payload.height, event.payload.codec);
      if (event.payload.source_id !== sourceId) return;
      if (initialized || initInProgress) return;

      initInProgress = true;

      const initData = base64ToArrayBuffer(event.payload.init_segment);
      const codec = `video/mp4; codecs="${event.payload.codec}"`;

      if (!MediaSource.isTypeSupported(codec)) {
        console.error('[VideoPlayer] Codec not supported:', codec);
        initInProgress = false;
        return;
      }

      mediaSource = new MediaSource();
      objectUrl = URL.createObjectURL(mediaSource);
      videoEl.src = objectUrl;

      mediaSource.addEventListener('sourceclose', () => console.warn('[VideoPlayer] MediaSource closed'));
      mediaSource.addEventListener('error', (e) => console.error('[VideoPlayer] MediaSource error:', e));
      mediaSource.addEventListener('sourceopen', () => {
        if (!mediaSource) return;
        try {
          sourceBuffer = mediaSource.addSourceBuffer(codec);
          sourceBuffer.mode = 'sequence'; // Better for live streaming
          sourceBuffer.addEventListener('updateend', onUpdateEnd);
          sourceBuffer.addEventListener('error', (e) => console.error('[VideoPlayer] SourceBuffer error:', e));
          appendBuffer(initData);
          initialized = true;
          console.log('[VideoPlayer] Ready to play');
        } catch (e) {
          console.error('[VideoPlayer] Failed to initialize:', e);
        }
      });
    });

    unlistenSegment = await listen<VideoSegmentEvent>('video-segment', (event) => {
      if (event.payload.source_id !== sourceId) return;

      const segmentData = base64ToArrayBuffer(event.payload.data);

      // If init is in progress but not complete, queue the segment
      if (!initialized) {
        if (initInProgress) {
          queue.push(segmentData);
        }
        return;
      }

      if (!sourceBuffer) return;
      appendBuffer(segmentData);
    });
  });

  onDestroy(() => {
    unlistenInit?.();
    unlistenSegment?.();
    if (objectUrl) {
      URL.revokeObjectURL(objectUrl);
    }
    if (mediaSource && mediaSource.readyState === 'open') {
      try { mediaSource.endOfStream(); } catch { /* ignore */ }
    }
  });
</script>

<video
  bind:this={videoEl}
  class="w-full h-full object-contain bg-black"
  muted
  playsinline
  onerror={(e) => console.error('[VideoPlayer] Error:', videoEl?.error)}
  onplaying={() => console.log('[VideoPlayer] Playing', videoEl?.videoWidth, 'x', videoEl?.videoHeight)}
  onpause={(e) => {
    // Auto-resume for live streaming (low latency)
    if (videoEl && sourceBuffer && sourceBuffer.buffered.length > 0) {
      const bufferedEnd = sourceBuffer.buffered.end(sourceBuffer.buffered.length - 1);
      if (bufferedEnd - videoEl.currentTime > 0.1) {
        console.log('[VideoPlayer] Auto-resuming playback');
        videoEl.play().catch(err => console.error('[VideoPlayer] Auto-resume failed:', err));
      }
    }
  }}
></video>
