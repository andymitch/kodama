<script lang="ts">
  import { Card, CardContent } from '$lib/components/ui/card/index.js';
  import { Badge } from '$lib/components/ui/badge/index.js';
  import VideoPlayer from './VideoPlayer.svelte';
  import AudioPlayer from './AudioPlayer.svelte';
  import AudioMeter from './AudioMeter.svelte';
  import TelemetryPanel from './TelemetryPanel.svelte';

  let { sourceId, name, connected }: {
    sourceId: string;
    name: string;
    connected: boolean;
  } = $props();
</script>

<Card>
  <div class="aspect-video bg-black rounded-t-lg overflow-hidden">
    <VideoPlayer {sourceId} />
  </div>
  <CardContent class="space-y-2 p-3">
    <div class="flex items-center justify-between">
      <span class="font-medium text-sm">{name}</span>
      <Badge variant={connected ? "success" : "secondary"}>
        {connected ? 'Live' : 'Offline'}
      </Badge>
    </div>
    <AudioMeter {sourceId} />
    <AudioPlayer {sourceId} />
    <TelemetryPanel {sourceId} />
  </CardContent>
</Card>
