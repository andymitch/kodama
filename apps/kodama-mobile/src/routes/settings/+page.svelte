<script lang="ts">
  import { Button } from '$lib/components/ui/button/index.js';
  import { Card, CardContent } from '$lib/components/ui/card/index.js';
  import { Input } from '$lib/components/ui/input/index.js';

  let bufferSize: number = $state(64);
  let autoReconnect: boolean = $state(true);
  let saved: boolean = $state(false);

  const isTauri = typeof window !== 'undefined' && '__TAURI__' in window;

  async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    if (!isTauri) throw new Error('Tauri not available');
    // @ts-ignore
    return window.__TAURI__.core.invoke(cmd, args);
  }

  async function loadSettings() {
    try {
      const settings = await invoke<{
        buffer_size: number;
        auto_reconnect: boolean;
      }>('get_settings');
      bufferSize = settings.buffer_size;
      autoReconnect = settings.auto_reconnect;
    } catch (_) {}
  }

  async function saveSettings() {
    try {
      await invoke('update_settings', { bufferSize, autoReconnect });
      saved = true;
      setTimeout(() => saved = false, 2000);
    } catch (_) {}
  }

  $effect(() => {
    loadSettings();
  });
</script>

<div class="min-h-dvh p-4 pb-20 pt-[env(safe-area-inset-top,1rem)]">
  <header class="mb-6">
    <h1 class="text-xl font-semibold">Settings</h1>
  </header>

  {#if saved}
    <div class="mb-4 rounded-lg bg-green-900/20 border border-green-800 p-3 text-center text-sm text-green-400">
      Settings saved
    </div>
  {/if}

  <div class="mb-6">
    <h2 class="mb-3 text-xs font-medium uppercase tracking-wide text-muted-foreground">
      Performance
    </h2>
    <Card>
      <CardContent class="flex items-center justify-between p-4">
        <div class="flex-1">
          <div class="font-medium">Buffer Size</div>
          <div class="text-sm text-muted-foreground">
            Frames to buffer (higher = more latency)
          </div>
        </div>
        <Input
          type="number"
          bind:value={bufferSize}
          min={16}
          max={256}
          class="w-20 text-center"
        />
      </CardContent>
    </Card>
  </div>

  <div class="mb-6">
    <h2 class="mb-3 text-xs font-medium uppercase tracking-wide text-muted-foreground">
      Connection
    </h2>
    <Card>
      <CardContent class="flex items-center justify-between p-4">
        <div class="flex-1">
          <div class="font-medium">Auto-reconnect</div>
          <div class="text-sm text-muted-foreground">
            Reconnect automatically on disconnect
          </div>
        </div>
        <button
          onclick={() => autoReconnect = !autoReconnect}
          class="relative h-7 w-12 rounded-full transition-colors {autoReconnect ? 'bg-primary' : 'bg-muted'}"
        >
          <span
            class="absolute top-1 h-5 w-5 rounded-full bg-white transition-transform {autoReconnect ? 'left-6' : 'left-1'}"
          ></span>
        </button>
      </CardContent>
    </Card>
  </div>

  <Button onclick={saveSettings} class="mb-8 h-12 w-full">
    Save Settings
  </Button>

  <div class="text-center">
    <h2 class="mb-3 text-xs font-medium uppercase tracking-wide text-muted-foreground">
      About
    </h2>
    <p class="text-sm">Kodama Mobile v0.1.0</p>
    <p class="text-sm text-muted-foreground">Privacy-focused P2P camera system</p>
  </div>
</div>
