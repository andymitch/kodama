<script lang="ts">
  import { Button } from '$lib/components/ui/button/index.js';
  import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '$lib/components/ui/card/index.js';
  import { Input } from '$lib/components/ui/input/index.js';

  let storagePath: string = $state('./recordings');
  let bufferSize: number = $state(64);
  let autoReconnect: boolean = $state(true);
  let saved: boolean = $state(false);

  const isTauri = typeof window !== 'undefined' && '__TAURI__' in window;

  async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    if (!isTauri) {
      throw new Error('Tauri not available');
    }
    // @ts-ignore
    return window.__TAURI__.core.invoke(cmd, args);
  }

  async function loadSettings() {
    try {
      const settings = await invoke<{
        storage_path: string;
        buffer_size: number;
        auto_reconnect: boolean;
      }>('get_settings');
      storagePath = settings.storage_path;
      bufferSize = settings.buffer_size;
      autoReconnect = settings.auto_reconnect;
    } catch (e) {
      console.error('Failed to load settings:', e);
    }
  }

  async function saveSettings() {
    try {
      await invoke('update_settings', {
        storagePath,
        bufferSize,
        autoReconnect
      });
      saved = true;
      setTimeout(() => saved = false, 2000);
    } catch (e) {
      console.error('Failed to save settings:', e);
    }
  }

  $effect(() => {
    loadSettings();
  });
</script>

<div class="max-w-xl space-y-6">
  <h1 class="text-2xl font-semibold">Settings</h1>

  {#if saved}
    <div class="rounded-lg bg-green-900/20 border border-green-800 p-4 text-green-400">
      Settings saved
    </div>
  {/if}

  <Card>
    <CardHeader>
      <CardTitle>Storage</CardTitle>
      <CardDescription>Configure where recordings are saved</CardDescription>
    </CardHeader>
    <CardContent class="space-y-4">
      <div class="space-y-2">
        <label for="storage-path" class="text-sm font-medium">Default storage path</label>
        <Input id="storage-path" bind:value={storagePath} />
        <p class="text-sm text-muted-foreground">
          Where recordings will be saved when storage is enabled
        </p>
      </div>
    </CardContent>
  </Card>

  <Card>
    <CardHeader>
      <CardTitle>Performance</CardTitle>
      <CardDescription>Tune streaming performance settings</CardDescription>
    </CardHeader>
    <CardContent class="space-y-4">
      <div class="space-y-2">
        <label for="buffer-size" class="text-sm font-medium">Broadcast buffer size</label>
        <Input id="buffer-size" type="number" bind:value={bufferSize} min={16} max={256} />
        <p class="text-sm text-muted-foreground">
          Number of frames to buffer (higher = more latency, lower drop rate)
        </p>
      </div>
    </CardContent>
  </Card>

  <Card>
    <CardHeader>
      <CardTitle>Connection</CardTitle>
      <CardDescription>Connection behavior settings</CardDescription>
    </CardHeader>
    <CardContent>
      <label class="flex items-center gap-3 cursor-pointer">
        <input
          type="checkbox"
          bind:checked={autoReconnect}
          class="h-4 w-4 rounded border-input"
        />
        <div>
          <span class="text-sm font-medium">Auto-reconnect on disconnect</span>
          <p class="text-sm text-muted-foreground">
            Automatically attempt to reconnect when connection is lost
          </p>
        </div>
      </label>
    </CardContent>
  </Card>

  <div class="border-t border-border pt-6">
    <Button onclick={saveSettings}>Save Settings</Button>
  </div>
</div>
