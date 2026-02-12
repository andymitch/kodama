<script lang="ts">
  import { getTransport } from '$lib/transport-ws.js';
  import type { TelemetryEvent } from '$lib/types.js';

  let { sourceId }: { sourceId: string } = $props();

  let telemetry: TelemetryEvent | null = $state(null);

  function formatUptime(secs: number): string {
    const h = Math.floor(secs / 3600);
    const m = Math.floor((secs % 3600) / 60);
    if (h > 24) {
      const d = Math.floor(h / 24);
      return `${d}d ${h % 24}h`;
    }
    return `${h}h ${m}m`;
  }

  function formatCoord(lat: number, lon: number): string {
    const latDir = lat >= 0 ? 'N' : 'S';
    const lonDir = lon >= 0 ? 'E' : 'W';
    return `${Math.abs(lat).toFixed(5)}${latDir} ${Math.abs(lon).toFixed(5)}${lonDir}`;
  }

  function formatSpeed(mps: number): string {
    const kmh = mps * 3.6;
    return `${kmh.toFixed(1)} km/h`;
  }

  function motionLabel(level: number): string {
    if (level < 0.1) return 'None';
    if (level < 0.3) return 'Low';
    if (level < 0.6) return 'Medium';
    return 'High';
  }

  function motionColor(level: number): string {
    if (level < 0.1) return 'text-muted-foreground';
    if (level < 0.3) return 'text-green-500';
    if (level < 0.6) return 'text-yellow-500';
    return 'text-red-500';
  }

  function handleTelemetry(event: TelemetryEvent) {
    if (event.source_id !== sourceId) return;
    telemetry = event;
  }

  $effect(() => {
    const transport = getTransport();
    const unlisten = transport.on('telemetry', handleTelemetry);
    return () => unlisten();
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
    {#if telemetry.motion_level !== null}
      <div class="flex justify-between">
        <span class="text-muted-foreground">Motion</span>
        <span class={motionColor(telemetry.motion_level)}>{motionLabel(telemetry.motion_level)}</span>
      </div>
    {/if}
    {#if telemetry.gps}
      <div class="flex justify-between col-span-2">
        <span class="text-muted-foreground">GPS</span>
        <span>{formatCoord(telemetry.gps.latitude, telemetry.gps.longitude)}</span>
      </div>
      {#if telemetry.gps.speed !== null}
        <div class="flex justify-between">
          <span class="text-muted-foreground">Speed</span>
          <span>{formatSpeed(telemetry.gps.speed)}</span>
        </div>
      {/if}
      {#if telemetry.gps.altitude !== null}
        <div class="flex justify-between">
          <span class="text-muted-foreground">Alt</span>
          <span>{telemetry.gps.altitude.toFixed(1)}m</span>
        </div>
      {/if}
    {/if}
  </div>
{:else}
  <div class="px-2 py-1 text-xs text-muted-foreground">Waiting for telemetry...</div>
{/if}
