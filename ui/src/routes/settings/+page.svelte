<script lang="ts">
  import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '$lib/components/ui/card/index.js';
  import { getTransport } from '$lib/transport-ws.js';
  import type { ServerStatus } from '$lib/types.js';
  import { onMount } from 'svelte';

  let status: ServerStatus | null = $state(null);

  onMount(async () => {
    const transport = getTransport();
    try {
      status = await transport.getStatus();
    } catch {
      // Server may not have /api/status yet
    }
  });
</script>

<div class="max-w-xl space-y-6">
  <h1 class="text-2xl font-semibold">Settings</h1>

  <Card>
    <CardHeader>
      <CardTitle>Server Status</CardTitle>
      <CardDescription>Current server information</CardDescription>
    </CardHeader>
    <CardContent class="space-y-4">
      {#if status}
        <div class="space-y-4 text-sm">
          {#if status.public_key}
            <div>
              <span class="text-muted-foreground">Server Key</span>
              <p class="font-mono text-xs break-all">{status.public_key}</p>
            </div>
          {/if}
          <div class="grid grid-cols-2 gap-4">
            <div>
              <span class="text-muted-foreground">Connected Cameras</span>
              <p class="font-medium">{status.cameras}</p>
            </div>
            <div>
              <span class="text-muted-foreground">Connected Clients</span>
              <p class="font-medium">{status.clients}</p>
            </div>
            <div>
              <span class="text-muted-foreground">Uptime</span>
              <p class="font-medium">
                {Math.floor(status.uptime_secs / 3600)}h {Math.floor((status.uptime_secs % 3600) / 60)}m
              </p>
            </div>
            <div>
              <span class="text-muted-foreground">Frames</span>
              <p class="font-medium">{status.frames_received} rx / {status.frames_broadcast} tx</p>
            </div>
          </div>
        </div>
      {:else}
        <p class="text-sm text-muted-foreground">Unable to load server status.</p>
      {/if}
    </CardContent>
  </Card>
</div>
