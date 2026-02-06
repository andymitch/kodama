<script lang="ts">
  import type { Snippet } from 'svelte';
  import { Video, Settings } from 'lucide-svelte';
  import '../app.css';

  let { children }: { children: Snippet } = $props();
</script>

<div class="flex min-h-dvh flex-col pb-[env(safe-area-inset-bottom)]">
  {@render children()}

  <nav class="fixed bottom-0 left-0 right-0 flex border-t border-border bg-card pb-[env(safe-area-inset-bottom)]">
    <a href="/" class="flex flex-1 flex-col items-center gap-1 py-3 text-muted-foreground transition-colors active:text-primary">
      <Video class="h-5 w-5" />
      <span class="text-xs font-medium">Cameras</span>
    </a>
    <a href="/settings" class="flex flex-1 flex-col items-center gap-1 py-3 text-muted-foreground transition-colors active:text-primary">
      <Settings class="h-5 w-5" />
      <span class="text-xs font-medium">Settings</span>
    </a>
  </nav>
</div>
