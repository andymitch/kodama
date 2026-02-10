<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { Badge } from '$lib/components/ui/badge/index.js';
  import CameraCard from '$lib/components/CameraCard.svelte';
  import { getTransport } from '$lib/transport-ws.js';
  import type { CameraInfo, CameraEvent } from '$lib/types.js';

  let cameras: CameraInfo[] = $state([]);
  let connected = $state(false);
  let error: string | null = $state(null);
  let unlistenCamera: (() => void) | null = null;

  function handleCameraEvent(event: CameraEvent) {
    if (event.connected) {
      if (!cameras.find(c => c.id === event.source_id)) {
        cameras = [...cameras, {
          id: event.source_id,
          name: `Camera ${event.source_id.slice(0, 8)}`,
          connected: true,
        }];
      }
    } else {
      cameras = cameras.filter(c => c.id !== event.source_id);
    }
  }

  onMount(async () => {
    const transport = getTransport();

    unlistenCamera = transport.on('camera-event', handleCameraEvent);

    try {
      await transport.connect();
      connected = true;

      // Load initial camera list
      try {
        const list = await transport.listCameras();
        if (list.length > 0) {
          cameras = list;
        }
      } catch {
        // Server may not have /api/cameras yet
      }
    } catch (e) {
      error = `Failed to connect to server: ${e}`;
    }
  });

  onDestroy(() => {
    unlistenCamera?.();
  });
</script>

<div class="space-y-6">
  <div class="flex items-center justify-between">
    <h1 class="text-2xl font-semibold">Dashboard</h1>
    <Badge variant={connected ? "success" : "secondary"}>
      {connected ? 'Connected' : 'Disconnected'}
    </Badge>
  </div>

  {#if error}
    <div class="rounded-lg border border-destructive/50 bg-destructive/10 p-4 text-destructive">
      {error}
    </div>
  {/if}

  {#if cameras.length === 0}
    <div class="rounded-lg border border-border bg-card p-12 text-center">
      <p class="text-muted-foreground">
        {#if connected}
          No cameras connected. Point cameras at this server to start streaming.
        {:else}
          Connecting to server...
        {/if}
      </p>
    </div>
  {:else}
    <div class="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
      {#each cameras as camera (camera.id)}
        <CameraCard
          sourceId={camera.id}
          name={camera.name}
          connected={camera.connected}
        />
      {/each}
    </div>
  {/if}
</div>
