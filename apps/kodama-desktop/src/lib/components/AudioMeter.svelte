<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import type { AudioLevelEvent } from '$lib/types';

  let { sourceId }: { sourceId: string } = $props();

  let level: number = $state(-60);
  let unlisten: (() => void) | null = null;

  // Map dB (-60 to 0) to percentage (0 to 100)
  function dbToPercent(db: number): number {
    return Math.max(0, Math.min(100, ((db + 60) / 60) * 100));
  }

  function levelColor(db: number): string {
    if (db > -6) return 'bg-red-500';
    if (db > -20) return 'bg-yellow-500';
    return 'bg-green-500';
  }

  onMount(async () => {
    if (!('__TAURI__' in window)) return;

    const { listen } = await import('@tauri-apps/api/event');

    unlisten = await listen<AudioLevelEvent>('audio-level', (event) => {
      if (event.payload.source_id !== sourceId) return;
      level = event.payload.level_db;
    });
  });

  onDestroy(() => {
    unlisten?.();
  });
</script>

<div class="flex items-center gap-2 px-2">
  <span class="text-xs text-muted-foreground w-6 shrink-0">🔊</span>
  <div class="flex-1 h-3 rounded-full bg-muted overflow-hidden">
    <div
      class="h-full rounded-full transition-all duration-100 {levelColor(level)}"
      style="width: {dbToPercent(level)}%"
    ></div>
  </div>
  <span class="text-xs text-muted-foreground w-12 text-right shrink-0">
    {level > -60 ? `${level.toFixed(0)} dB` : 'Silent'}
  </span>
</div>
