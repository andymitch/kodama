<script lang="ts">
  import { Button } from '$lib/components/ui/button/index.js';
  import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '$lib/components/ui/card/index.js';
  import { Input } from '$lib/components/ui/input/index.js';

  let storagePath: string = $state('./recordings');
  let bufferSize: number = $state(64);
  let defaultMode: string = $state('idle');
  let serverKey: string = $state('');
  let storageEnabled: boolean = $state(false);
  let maxGb: number = $state(10);
  let retentionDays: number = $state(7);
  let keyframesOnly: boolean = $state(false);
  let saved: boolean = $state(false);

  const isTauri = typeof window !== 'undefined' && '__TAURI__' in window;

  async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    if (!isTauri) {
      throw new Error('Tauri not available');
    }
    // @ts-ignore
    return window.__TAURI__.core.invoke(cmd, args);
  }

  interface AppSettings {
    default_mode: string;
    server_key: string | null;
    storage: {
      enabled: boolean;
      path: string | null;
      max_gb: number;
      retention_days: number;
      keyframes_only: boolean;
    };
    buffer_size: number;
  }

  async function loadSettings() {
    try {
      const settings = await invoke<AppSettings>('get_settings');
      defaultMode = settings.default_mode;
      serverKey = settings.server_key ?? '';
      storageEnabled = settings.storage.enabled;
      storagePath = settings.storage.path ?? './recordings';
      maxGb = settings.storage.max_gb;
      retentionDays = settings.storage.retention_days;
      keyframesOnly = settings.storage.keyframes_only;
      bufferSize = settings.buffer_size;
    } catch (e) {
      console.error('Failed to load settings:', e);
    }
  }

  async function saveSettings() {
    try {
      await invoke('save_settings', {
        settings: {
          default_mode: defaultMode,
          server_key: serverKey || null,
          storage: {
            enabled: storageEnabled,
            path: storagePath || null,
            max_gb: maxGb,
            retention_days: retentionDays,
            keyframes_only: keyframesOnly
          },
          buffer_size: bufferSize
        }
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

  <div class="border-t border-border pt-6">
    <Button onclick={saveSettings}>Save Settings</Button>
  </div>
</div>
