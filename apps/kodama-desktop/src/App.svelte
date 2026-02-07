<script lang="ts">
  import Dashboard from './pages/Dashboard.svelte';
  import Settings from './pages/Settings.svelte';

  let currentPage: 'dashboard' | 'settings' = $state('dashboard');
</script>

<div class="flex min-h-screen flex-col">
  <header class="border-b border-border bg-card px-6 py-3">
    <nav class="mx-auto flex max-w-6xl items-center justify-between">
      <span class="text-xl font-semibold text-primary">Kodama</span>
      <div class="flex gap-6">
        <button
          class="text-sm transition-colors hover:text-foreground {currentPage === 'dashboard' ? 'text-foreground' : 'text-muted-foreground'}"
          onclick={() => currentPage = 'dashboard'}
        >
          Dashboard
        </button>
        <button
          class="text-sm transition-colors hover:text-foreground {currentPage === 'settings' ? 'text-foreground' : 'text-muted-foreground'}"
          onclick={() => currentPage = 'settings'}
        >
          Settings
        </button>
      </div>
    </nav>
  </header>

  <main class="mx-auto w-full max-w-6xl flex-1 p-6">
    {#if currentPage === 'dashboard'}
      <Dashboard />
    {:else}
      <Settings />
    {/if}
  </main>
</div>
