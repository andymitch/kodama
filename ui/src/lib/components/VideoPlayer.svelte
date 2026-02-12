<script lang="ts">
  import { getTransport } from '$lib/transport-ws.js';
  import type { VideoInitEvent, VideoSegmentEvent } from '$lib/types.js';

  let { sourceId }: { sourceId: string } = $props();

  let videoEl: HTMLVideoElement;
  let mediaSource: MediaSource | null = null;
  let sourceBuffer: SourceBuffer | null = null;
  let objectUrl: string = '';
  let queue: ArrayBuffer[] = [];
  const MAX_QUEUE_SIZE = 30;
  let initialized = false;
  let initInProgress = false;
  let playStarted = false;
  let mediaSegmentsAppended = 0;
  let appendErrorCount = 0;
  let droppedSegments = 0;
  let liveEdgeTimer: ReturnType<typeof setInterval> | null = null;

  function tryPlay() {
    if (playStarted || !videoEl) return;
    playStarted = true;
    videoEl.play().catch((e) => {
      console.warn('[VideoPlayer] play() rejected:', e);
      playStarted = false;
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
      if (queue.length >= MAX_QUEUE_SIZE) {
        queue.shift();
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

    if (!playStarted && sourceBuffer.buffered.length > 0) {
      const bufferedDuration = sourceBuffer.buffered.end(0) - sourceBuffer.buffered.start(0);
      if (bufferedDuration > 0.05) {
        tryPlay();
      }
    }

    if (videoEl && playStarted && sourceBuffer.buffered.length > 0) {
      const currentTime = videoEl.currentTime;
      const bufferedEnd = sourceBuffer.buffered.end(sourceBuffer.buffered.length - 1);
      const behind = bufferedEnd - currentTime;

      if (behind > 2) {
        videoEl.currentTime = bufferedEnd - 0.3;
      }

      if (videoEl.paused && behind > 0.1) {
        videoEl.play().catch(() => {});
      }
    }

    if (videoEl && sourceBuffer.buffered.length > 0) {
      const currentTime = videoEl.currentTime;
      const start = sourceBuffer.buffered.start(0);
      const end = sourceBuffer.buffered.end(sourceBuffer.buffered.length - 1);

      if (currentTime - start > 5) {
        const removeEnd = Math.min(currentTime - 1, end - 0.1);
        if (removeEnd > start) {
          try {
            sourceBuffer.remove(start, removeEnd);
            return;
          } catch (e) {
            console.error('[VideoPlayer] Failed to remove buffer:', e);
          }
        }
      }
    }

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

  function handleInit(event: VideoInitEvent) {
    if (event.source_id !== sourceId) return;
    if (initInProgress) return;

    if (initialized) {
      if (sourceBuffer) {
        try { sourceBuffer.removeEventListener('updateend', onUpdateEnd); } catch {}
        sourceBuffer = null;
      }
      if (mediaSource && mediaSource.readyState === 'open') {
        try { mediaSource.endOfStream(); } catch {}
      }
      if (objectUrl) {
        URL.revokeObjectURL(objectUrl);
        objectUrl = '';
      }
      initialized = false;
      initInProgress = false;
      playStarted = false;
      queue = [];
      mediaSegmentsAppended = 0;
      appendErrorCount = 0;
      if (liveEdgeTimer) { clearInterval(liveEdgeTimer); liveEdgeTimer = null; }
    }

    initInProgress = true;

    const initData = event.init_segment;
    const codec = `video/mp4; codecs="${event.codec}"`;

    if (!MediaSource.isTypeSupported(codec)) {
      console.error('[VideoPlayer] Codec not supported:', codec);
      initInProgress = false;
      return;
    }

    mediaSource = new MediaSource();
    objectUrl = URL.createObjectURL(mediaSource);
    videoEl.src = objectUrl;

    mediaSource.addEventListener('sourceopen', () => {
      if (!mediaSource) return;
      try {
        sourceBuffer = mediaSource.addSourceBuffer(codec);
        sourceBuffer.mode = 'sequence';
        sourceBuffer.addEventListener('updateend', onUpdateEnd);
        appendBuffer(initData);
        initialized = true;
        initInProgress = false;

        if (liveEdgeTimer) clearInterval(liveEdgeTimer);
        liveEdgeTimer = setInterval(() => {
          if (!videoEl || !sourceBuffer || sourceBuffer.buffered.length === 0) return;
          const bufferedEnd = sourceBuffer.buffered.end(sourceBuffer.buffered.length - 1);
          const behind = bufferedEnd - videoEl.currentTime;
          if (behind > 3) {
            videoEl.currentTime = bufferedEnd - 0.3;
            if (videoEl.paused) {
              videoEl.play().catch(() => {});
            }
          }
        }, 1000);
      } catch (e) {
        console.error('[VideoPlayer] Failed to initialize:', e);
      }
    });
  }

  function handleSegment(event: VideoSegmentEvent) {
    if (event.source_id !== sourceId) return;

    if (!initialized) {
      if (initInProgress) {
        queue.push(event.data);
      }
      return;
    }

    if (!sourceBuffer) return;
    appendBuffer(event.data);
  }

  $effect(() => {
    const transport = getTransport();
    const unlistenInit = transport.on('video-init', handleInit);
    const unlistenSegment = transport.on('video-segment', handleSegment);

    return () => {
      unlistenInit();
      unlistenSegment();
      if (liveEdgeTimer) { clearInterval(liveEdgeTimer); liveEdgeTimer = null; }
      if (objectUrl) {
        URL.revokeObjectURL(objectUrl);
      }
      if (mediaSource && mediaSource.readyState === 'open') {
        try { mediaSource.endOfStream(); } catch {}
      }
    };
  });
</script>

<video
  bind:this={videoEl}
  class="w-full h-full object-contain bg-black"
  muted
  playsinline
  onwaiting={() => {
    if (videoEl && sourceBuffer && sourceBuffer.buffered.length > 0) {
      const bufferedEnd = sourceBuffer.buffered.end(sourceBuffer.buffered.length - 1);
      const behind = bufferedEnd - videoEl.currentTime;
      if (behind > 0.5) {
        videoEl.currentTime = bufferedEnd - 0.1;
      }
    }
  }}
  onpause={() => {
    if (videoEl && sourceBuffer && sourceBuffer.buffered.length > 0) {
      const bufferedEnd = sourceBuffer.buffered.end(sourceBuffer.buffered.length - 1);
      if (bufferedEnd - videoEl.currentTime > 0.1) {
        videoEl.play().catch(() => {});
      }
    }
  }}
></video>
