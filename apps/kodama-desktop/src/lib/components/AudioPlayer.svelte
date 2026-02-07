<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import type { AudioDataEvent } from '$lib/types';

  let { sourceId }: { sourceId: string } = $props();

  let audioContext: AudioContext | null = null;
  let unlisten: (() => void) | null = null;
  let nextPlayTime = 0;
  let isPlaying = false;

  function base64ToArrayBuffer(base64: string): ArrayBuffer {
    const binary = atob(base64);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) {
      bytes[i] = binary.charCodeAt(i);
    }
    return bytes.buffer;
  }

  function playAudioChunk(pcmData: ArrayBuffer, sampleRate: number, channels: number) {
    if (!audioContext) {
      audioContext = new AudioContext();
      nextPlayTime = audioContext.currentTime;
      isPlaying = true;
      console.log('[AudioPlayer] AudioContext created, sampleRate:', audioContext.sampleRate);
    }

    // Convert PCM s16le to Float32Array
    const int16Array = new Int16Array(pcmData);
    const floatArray = new Float32Array(int16Array.length);
    for (let i = 0; i < int16Array.length; i++) {
      floatArray[i] = int16Array[i] / 32768.0; // Convert to -1.0 to 1.0 range
    }

    // Create audio buffer
    const audioBuffer = audioContext.createBuffer(channels, floatArray.length, sampleRate);
    audioBuffer.getChannelData(0).set(floatArray);

    // Create source node and schedule playback
    const source = audioContext.createBufferSource();
    source.buffer = audioBuffer;
    source.connect(audioContext.destination);

    // Schedule at the next play time for smooth continuous audio
    const scheduleTime = Math.max(nextPlayTime, audioContext.currentTime);
    source.start(scheduleTime);
    nextPlayTime = scheduleTime + audioBuffer.duration;
  }

  onMount(async () => {
    if (!('__TAURI__' in window)) return;

    const { listen } = await import('@tauri-apps/api/event');

    unlisten = await listen<AudioDataEvent>('audio-data', (event) => {
      if (event.payload.source_id !== sourceId) return;

      try {
        const pcmData = base64ToArrayBuffer(event.payload.data);
        playAudioChunk(pcmData, event.payload.sample_rate, event.payload.channels);
      } catch (e) {
        console.error('[AudioPlayer] Failed to play audio:', e);
      }
    });

    console.log('[AudioPlayer] Listening for audio from:', sourceId);
  });

  onDestroy(() => {
    unlisten?.();
    if (audioContext) {
      audioContext.close();
    }
  });
</script>

<!-- Audio player is invisible - just plays audio -->
<div class="hidden"></div>
