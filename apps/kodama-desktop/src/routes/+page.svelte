<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { Button } from '$lib/components/ui/button/index.js';
  import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '$lib/components/ui/card/index.js';
  import { Input } from '$lib/components/ui/input/index.js';
  import { Badge } from '$lib/components/ui/badge/index.js';
  import { Server, MonitorPlay } from 'lucide-svelte';
  import CameraCard from '$lib/components/CameraCard.svelte';
  import type { CameraEvent } from '$lib/types';

  type AppMode = 'idle' | 'server' | 'client';
  type ConnectionState = 'disconnected' | 'connecting' | 'connected';

  let mode: AppMode = $state('idle');
  let connectionState: ConnectionState = $state('disconnected');
  let serverKey: string = $state('');
  let remoteServerKey: string = $state('');
  let storageEnabled: boolean = $state(true);
  let storagePath: string = $state('./recordings');
  let cameras: Array<{ id: string; name: string; connected: boolean }> = $state([]);
  let error: string | null = $state(null);

  let unlistenCamera: (() => void) | null = null;

  function isTauri(): boolean {
    return typeof window !== 'undefined' && '__TAURI__' in window;
  }

  async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    if (!isTauri()) {
      console.warn('Tauri not available, running in browser mode');
      throw new Error('Tauri not available');
    }
    // @ts-ignore
    return window.__TAURI__.core.invoke(cmd, args);
  }

  async function startServer() {
    error = null;
    connectionState = 'connecting';
    try {
      const key = await invoke<string>('start_server', {
        storageConfig: storageEnabled ? {
          enabled: true,
          path: storagePath,
          max_gb: 10,
          retention_days: 7,
          keyframes_only: false
        } : null
      });
      serverKey = key;
      mode = 'server';
      connectionState = 'connected';
    } catch (e) {
      error = `Failed to start server: ${e}`;
      connectionState = 'disconnected';
    }
  }

  async function stopServer() {
    error = null;
    try {
      await invoke('stop_server');
      mode = 'idle';
      connectionState = 'disconnected';
      serverKey = '';
      cameras = [];
    } catch (e) {
      error = `Failed to stop server: ${e}`;
    }
  }

  async function connectToServer() {
    if (!remoteServerKey.trim()) {
      error = 'Please enter a server key';
      return;
    }
    error = null;
    connectionState = 'connecting';
    try {
      await invoke('connect_to_server', { serverKey: remoteServerKey });
      mode = 'client';
      connectionState = 'connected';
    } catch (e) {
      error = `Failed to connect: ${e}`;
      connectionState = 'disconnected';
    }
  }

  async function disconnect() {
    error = null;
    try {
      await invoke('disconnect');
      mode = 'idle';
      connectionState = 'disconnected';
      cameras = [];
    } catch (e) {
      error = `Failed to disconnect: ${e}`;
    }
  }

  async function refreshCameras() {
    try {
      const list = await invoke<Array<{ id: string; name: string; connected: boolean }>>('list_cameras');
      if (list.length > 0) {
        cameras = list;
      }
    } catch { /* not connected yet */ }
  }

  onMount(async () => {
    if (!isTauri()) return;

    const { listen } = await import('@tauri-apps/api/event');

    // Restore state on mount (handles HMR/page reload)
    try {
      const status = await invoke<{ running: boolean; public_key: string | null }>('get_server_status');
      console.log('[Page] get_server_status:', JSON.stringify(status));
      if (status.running && status.public_key) {
        mode = 'server';
        connectionState = 'connected';
        serverKey = status.public_key;
        error = null;
      }
    } catch (e) { console.error('[Page] get_server_status error:', e); }
    try {
      const conn = await invoke<{ connected: boolean; mode: string; server_key: string | null }>('get_connection_status');
      console.log('[Page] get_connection_status:', JSON.stringify(conn));
      if (conn.connected && conn.mode === 'client') {
        mode = 'client';
        connectionState = 'connected';
        error = null;
      }
    } catch (e) { console.error('[Page] get_connection_status error:', e); }
    await refreshCameras();
    console.log('[Page] cameras after refresh:', cameras.length);

    unlistenCamera = await listen<CameraEvent>('camera-event', (event) => {
      const { source_id, connected } = event.payload;
      if (connected) {
        // Add camera if not already present
        if (!cameras.find(c => c.id === source_id)) {
          cameras = [...cameras, {
            id: source_id,
            name: `Camera ${source_id.slice(0, 8)}`,
            connected: true
          }];
        }
      } else {
        // Remove disconnected camera
        cameras = cameras.filter(c => c.id !== source_id);
      }
    });
  });

  onDestroy(() => {
    unlistenCamera?.();
  });
</script>

<div class="space-y-6">
  <h1 class="text-2xl font-semibold">Dashboard</h1>

  {#if error}
    <div class="rounded-lg border border-destructive/50 bg-destructive/10 p-4 text-destructive">
      {error}
    </div>
  {/if}

  {#if mode === 'idle'}
    <div class="grid gap-6 md:grid-cols-2">
      <Card>
        <CardHeader>
          <div class="flex items-center gap-3">
            <Server class="h-5 w-5 text-primary" />
            <CardTitle class="text-lg">Server Mode</CardTitle>
          </div>
          <CardDescription>
            Host your own camera server and receive streams from cameras on your network.
          </CardDescription>
        </CardHeader>
        <CardContent class="space-y-4">
          <label class="flex items-center gap-2 text-sm">
            <input type="checkbox" bind:checked={storageEnabled} class="rounded" />
            Enable recording storage
          </label>

          {#if storageEnabled}
            <div class="space-y-2">
              <label for="storage-path" class="text-sm text-muted-foreground">Storage path</label>
              <Input id="storage-path" bind:value={storagePath} />
            </div>
          {/if}

          <Button onclick={startServer} class="w-full">Start Server</Button>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <div class="flex items-center gap-3">
            <MonitorPlay class="h-5 w-5 text-primary" />
            <CardTitle class="text-lg">Client Mode</CardTitle>
          </div>
          <CardDescription>
            Connect to an existing Kodama server to view camera streams.
          </CardDescription>
        </CardHeader>
        <CardContent class="space-y-4">
          <div class="space-y-2">
            <label for="server-key" class="text-sm text-muted-foreground">Server key</label>
            <Input id="server-key" bind:value={remoteServerKey} placeholder="Enter base32 server key" />
          </div>

          <Button onclick={connectToServer} class="w-full">Connect</Button>
        </CardContent>
      </Card>
    </div>

  {:else if mode === 'server'}
    <div class="space-y-6">
      <div class="flex items-center gap-4">
        <Badge variant="success">Server Running</Badge>
        <Button variant="outline" onclick={stopServer}>Stop Server</Button>
      </div>

      <Card>
        <CardHeader>
          <CardTitle class="text-base">Server Key</CardTitle>
          <CardDescription>Share this key with cameras and clients to connect</CardDescription>
        </CardHeader>
        <CardContent>
          <code class="block rounded bg-background p-3 font-mono text-sm text-primary break-all">
            {serverKey}
          </code>
        </CardContent>
      </Card>

      <div>
        <h2 class="mb-4 text-lg font-medium text-muted-foreground">
          Connected Cameras ({cameras.length})
        </h2>
        {#if cameras.length === 0}
          <p class="text-muted-foreground">
            No cameras connected. Share your server key with cameras to connect.
          </p>
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
    </div>

  {:else if mode === 'client'}
    <div class="space-y-6">
      <div class="flex items-center gap-4">
        <Badge variant="success">Connected</Badge>
        <Button variant="outline" onclick={disconnect}>Disconnect</Button>
      </div>

      <div>
        <h2 class="mb-4 text-lg font-medium text-muted-foreground">
          Available Cameras ({cameras.length})
        </h2>
        {#if cameras.length === 0}
          <p class="text-muted-foreground">No cameras available on this server.</p>
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
    </div>
  {/if}
</div>
