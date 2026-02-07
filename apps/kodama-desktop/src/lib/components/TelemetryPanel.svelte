<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import type { TelemetryEvent } from '$lib/types';

  let { sourceId }: { sourceId: string } = $props();

  let telemetry: TelemetryEvent | null = $state(null);
  let unlisten: (() => void) | null = null;

  function formatUptime(secs: number): string {
    const h = Math.floor(secs / 3600);
    const m = Math.floor((secs % 3600) / 60);
    if (h > 24) {
      const d = Math.floor(h / 24);
      return `${d}d ${h % 24}h`;
    }
    return `${h}h ${m}m`;
  }

  onMount(async () => {
    if (!('__TAURI__' in window)) return;

    const { listen } = await import('@tauri-apps/api/event');

    unlisten = await listen<TelemetryEvent>('telemetry', (event) => {
      if (event.payload.source_id !== sourceId) return;
      telemetry = event.payload;
    });
  });

  onDestroy(() => {
    unlisten?.();
  });
</script>

{#if telemetry}
  <div class="grid grid-cols-2 gap-x-4 gap-y-1 px-2 py-1 text-xs">
    <div class="flex justify-between">
      <span class="text-muted-foreground">CPU</span>
      <span>{telemetry.cpu_usage.toFixed(1)}%</span>
    </div>
    <div class="flex justify-between">
      <span class="text-muted-foreground">Mem</span>
      <span>{telemetry.memory_usage.toFixed(1)}%</span>
    </div>
    {#if telemetry.cpu_temp !== null}
      <div class="flex justify-between">
        <span class="text-muted-foreground">Temp</span>
        <span>{telemetry.cpu_temp.toFixed(1)}°C</span>
      </div>
    {/if}
    <div class="flex justify-between">
      <span class="text-muted-foreground">Disk</span>
      <span>{telemetry.disk_usage.toFixed(1)}%</span>
    </div>
    <div class="flex justify-between">
      <span class="text-muted-foreground">Load</span>
      <span>{telemetry.load_average.map(v => v.toFixed(2)).join(' ')}</span>
    </div>
    <div class="flex justify-between">
      <span class="text-muted-foreground">Up</span>
      <span>{formatUptime(telemetry.uptime_secs)}</span>
    </div>
  </div>
{:else}
  <div class="px-2 py-1 text-xs text-muted-foreground">Waiting for telemetry...</div>
{/if}
