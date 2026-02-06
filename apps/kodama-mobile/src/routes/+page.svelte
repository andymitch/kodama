<script lang="ts">
  import { onMount } from 'svelte';
  import { Button } from '$lib/components/ui/button/index.js';
  import { Card, CardContent } from '$lib/components/ui/card/index.js';
  import { Input } from '$lib/components/ui/input/index.js';
  import { Badge } from '$lib/components/ui/badge/index.js';
  import { Server, Video } from 'lucide-svelte';

  type AppMode = 'idle' | 'server' | 'client';
  type ConnectionState = 'disconnected' | 'connecting' | 'connected';

  let mode: AppMode = $state('idle');
  let connectionState: ConnectionState = $state('disconnected');
  let serverKey: string = $state('');
  let remoteServerKey: string = $state('');
  let cameras: Array<{ id: string; name: string; connected: boolean }> = $state([]);
  let error: string | null = $state(null);

  const isTauri = typeof window !== 'undefined' && '__TAURI__' in window;

  async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    if (!isTauri) {
      throw new Error('Tauri not available');
    }
    // @ts-ignore
    return window.__TAURI__.core.invoke(cmd, args);
  }

  async function startServer() {
    error = null;
    connectionState = 'connecting';
    try {
      const key = await invoke<string>('start_server');
      serverKey = key;
      mode = 'server';
      connectionState = 'connected';
    } catch (e) {
      error = `${e}`;
      connectionState = 'disconnected';
    }
  }

  async function stopServer() {
    try {
      await invoke('stop_server');
      mode = 'idle';
      connectionState = 'disconnected';
      serverKey = '';
      cameras = [];
    } catch (e) {
      error = `${e}`;
    }
  }

  async function connectToServer() {
    if (!remoteServerKey.trim()) {
      error = 'Enter a server key';
      return;
    }
    error = null;
    connectionState = 'connecting';
    try {
      await invoke('connect_to_server', { serverKey: remoteServerKey });
      mode = 'client';
      connectionState = 'connected';
    } catch (e) {
      error = `${e}`;
      connectionState = 'disconnected';
    }
  }

  async function disconnect() {
    try {
      await invoke('disconnect');
      mode = 'idle';
      connectionState = 'disconnected';
      cameras = [];
    } catch (e) {
      error = `${e}`;
    }
  }

  async function refreshCameras() {
    try {
      cameras = await invoke('get_cameras');
    } catch (_) {}
  }

  onMount(() => {
    const interval = setInterval(() => {
      if (connectionState === 'connected') refreshCameras();
    }, 2000);
    return () => clearInterval(interval);
  });
</script>

<div class="min-h-dvh p-4 pb-20 pt-[env(safe-area-inset-top,1rem)]">
  <header class="mb-6 flex items-center gap-3">
    <h1 class="text-xl font-semibold">Kodama</h1>
    {#if mode !== 'idle'}
      <Badge variant="success">{mode === 'server' ? 'Server' : 'Client'}</Badge>
    {/if}
  </header>

  {#if error}
    <div class="mb-4 rounded-lg border border-destructive/50 bg-destructive/10 p-3 text-sm text-destructive">
      {error}
    </div>
  {/if}

  {#if mode === 'idle'}
    <div class="space-y-6">
      <button
        onclick={startServer}
        class="flex w-full flex-col items-center gap-2 rounded-xl border border-border bg-card p-6 transition-colors active:border-primary active:bg-card/80"
      >
        <Server class="h-8 w-8 text-primary" />
        <span class="text-lg font-semibold">Start Server</span>
        <span class="text-sm text-muted-foreground">Host cameras on this device</span>
      </button>

      <div class="text-center text-sm text-muted-foreground">or</div>

      <div class="space-y-3">
        <Input
          bind:value={remoteServerKey}
          placeholder="Enter server key"
          class="h-12 text-base"
        />
        <Button onclick={connectToServer} class="h-12 w-full text-base">
          Connect
        </Button>
      </div>
    </div>

  {:else if mode === 'server'}
    <div class="space-y-6">
      <Card>
        <CardContent class="p-4">
          <div class="mb-1 text-xs text-muted-foreground">Server Key</div>
          <code class="block break-all font-mono text-sm text-primary">
            {serverKey}
          </code>
          <div class="mt-2 text-xs text-muted-foreground">
            Share this with cameras and clients
          </div>
        </CardContent>
      </Card>

      <div>
        <h2 class="mb-3 text-sm font-medium text-muted-foreground">
          Cameras ({cameras.length})
        </h2>
        {#if cameras.length === 0}
          <p class="py-8 text-center text-sm text-muted-foreground">
            No cameras connected
          </p>
        {:else}
          <div class="space-y-3">
            {#each cameras as camera}
              <Card>
                <div class="aspect-[16/10] bg-background flex items-center justify-center">
                  <Video class="h-6 w-6 text-muted-foreground/30" />
                </div>
                <CardContent class="p-3">
                  <span class="font-medium">{camera.name}</span>
                </CardContent>
              </Card>
            {/each}
          </div>
        {/if}
      </div>

      <Button variant="destructive" onclick={stopServer} class="h-12 w-full">
        Stop Server
      </Button>
    </div>

  {:else if mode === 'client'}
    <div class="space-y-6">
      <div>
        <h2 class="mb-3 text-sm font-medium text-muted-foreground">
          Cameras ({cameras.length})
        </h2>
        {#if cameras.length === 0}
          <p class="py-8 text-center text-sm text-muted-foreground">
            No cameras available
          </p>
        {:else}
          <div class="space-y-3">
            {#each cameras as camera}
              <Card>
                <div class="aspect-video bg-background flex items-center justify-center">
                  <Video class="h-8 w-8 text-muted-foreground/30" />
                </div>
                <CardContent class="p-3">
                  <span class="font-medium">{camera.name}</span>
                </CardContent>
              </Card>
            {/each}
          </div>
        {/if}
      </div>

      <Button variant="destructive" onclick={disconnect} class="h-12 w-full">
        Disconnect
      </Button>
    </div>
  {/if}
</div>
