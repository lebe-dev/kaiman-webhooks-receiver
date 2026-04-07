<script lang="ts">
  import { SvelteSet } from "svelte/reactivity";
  import {
    fetchQueue,
    pauseForwarding,
    resumeForwarding,
    clearQueue,
    retryWebhook,
    deleteWebhook,
    type QueueItem,
    type ChannelForwardStatus,
  } from "$lib/api";
  import { Button } from "$lib/components/ui/button";
  import { toast } from "svelte-sonner";
  import { RotateCw, Pause, Play, Trash2, RefreshCw } from "@lucide/svelte";

  let { channel }: { channel: string } = $props();

  let items = $state<QueueItem[]>([]);
  let status = $state<ChannelForwardStatus>({
    paused: false,
    queue_size: 0,
    last_success_at: null,
    last_error_at: null,
    last_error_message: null,
  });
  let loading = $state(false);
  let expanded = new SvelteSet<number>();
  let retrying = $state<number | null>(null);

  let lastItemsJson = "";

  async function load(silent = false) {
    if (!silent) loading = true;
    try {
      const res = await fetchQueue(channel);
      const newJson = JSON.stringify(res.items);
      if (newJson !== lastItemsJson) {
        items = res.items;
        lastItemsJson = newJson;
      }
      status = res.status;
    } catch {
      toast.error("Failed to load queue");
    } finally {
      loading = false;
    }
  }

  function toggleExpand(id: number) {
    if (expanded.has(id)) {
      expanded.delete(id);
    } else {
      expanded.add(id);
    }
  }

  function formatDate(ts: number): string {
    return new Date(ts * 1000).toLocaleString();
  }

  function relativeTime(ts: number | null): string {
    if (ts === null) return "never";
    const diff = Math.floor(Date.now() / 1000) - ts;
    if (diff < 60) return `${diff}s ago`;
    if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
    if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
    return formatDate(ts);
  }

  async function handlePauseResume() {
    try {
      if (status.paused) {
        await resumeForwarding(channel);
        toast.success("Forwarding resumed");
      } else {
        await pauseForwarding(channel);
        toast.success("Forwarding paused");
      }
      await load();
    } catch {
      toast.error("Failed to toggle forwarding state");
    }
  }

  async function handleClear() {
    if (!confirm("Clear all items from the queue?")) return;
    try {
      await clearQueue(channel);
      items = [];
      toast.success("Queue cleared");
      await load();
    } catch {
      toast.error("Failed to clear queue");
    }
  }

  async function handleRetry(id: number) {
    retrying = id;
    try {
      const result = await retryWebhook(channel, id);
      if (result.success) {
        items = items.filter((i) => i.id !== id);
        toast.success(`Webhook #${id} forwarded successfully (${result.status_code})`);
      } else {
        toast.error(result.error ?? "Retry failed");
        await load();
      }
    } catch {
      toast.error("Failed to retry webhook");
      await load();
    } finally {
      retrying = null;
    }
  }

  async function handleDelete(id: number) {
    if (!confirm(`Delete webhook #${id}?`)) return;
    try {
      await deleteWebhook(channel, id);
      items = items.filter((i) => i.id !== id);
      toast.success("Webhook deleted");
    } catch {
      toast.error("Failed to delete webhook");
    }
  }

  $effect(() => {
    void channel;
    expanded.clear();
    lastItemsJson = "";
    load();
  });

  $effect(() => {
    const interval = setInterval(() => load(true), 5000);
    return () => clearInterval(interval);
  });
</script>

<div class="space-y-4">
  <!-- Status panel -->
  <div class="grid grid-cols-2 md:grid-cols-4 gap-3">
    <div class="rounded-lg border p-3 space-y-1">
      <div class="text-xs text-muted-foreground">Queue size</div>
      <div class="text-2xl font-semibold">{status.queue_size}</div>
    </div>
    <div class="rounded-lg border p-3 space-y-1">
      <div class="text-xs text-muted-foreground">Status</div>
      <div class="text-sm font-medium">
        {#if status.paused}
          <span class="inline-flex items-center gap-1.5 text-yellow-600 dark:text-yellow-400">
            <span class="w-2 h-2 rounded-full bg-yellow-500"></span>
            Paused
          </span>
        {:else}
          <span class="inline-flex items-center gap-1.5 text-green-600 dark:text-green-400">
            <span class="w-2 h-2 rounded-full bg-green-500"></span>
            Active
          </span>
        {/if}
      </div>
    </div>
    <div class="rounded-lg border p-3 space-y-1">
      <div class="text-xs text-muted-foreground">Last success</div>
      <div class="text-sm font-medium">{relativeTime(status.last_success_at)}</div>
    </div>
    <div class="rounded-lg border p-3 space-y-1">
      <div class="text-xs text-muted-foreground">Last error</div>
      <div class="text-sm font-medium">
        {relativeTime(status.last_error_at)}
      </div>
      {#if status.last_error_message}
        <div class="text-xs text-destructive truncate" title={status.last_error_message}>
          {status.last_error_message}
        </div>
      {/if}
    </div>
  </div>

  <!-- Controls row -->
  <div class="flex items-center gap-2 flex-wrap">
    <Button
      variant="outline"
      size="sm"
      onclick={load}
      disabled={loading}
    >
      <RotateCw size={16} class={loading ? "animate-spin" : ""} />
    </Button>
    <Button
      variant="outline"
      size="sm"
      onclick={handlePauseResume}
    >
      {#if status.paused}
        <Play size={16} />
        <span class="ml-1">Resume</span>
      {:else}
        <Pause size={16} />
        <span class="ml-1">Pause</span>
      {/if}
    </Button>
    {#if items.length > 0}
      <Button
        variant="destructive"
        size="sm"
        onclick={handleClear}
      >
        <Trash2 size={16} />
        <span class="ml-1">Clear Queue</span>
      </Button>
    {/if}
    <span class="text-sm text-muted-foreground">
      {items.length} item{items.length !== 1 ? "s" : ""}
    </span>
  </div>

  <!-- Queue list -->
  {#if items.length === 0 && !loading}
    <p class="text-sm text-muted-foreground">Queue is empty.</p>
  {:else}
    <div class="space-y-3">
      {#each items as item (item.id)}
        <div class="rounded-lg border">
          <!-- svelte-ignore a11y_click_events_have_key_events -->
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div
            class="p-4 cursor-pointer select-none"
            onclick={() => toggleExpand(item.id)}
          >
            <div class="flex items-center justify-between gap-2">
              <div class="flex items-center gap-2 flex-wrap min-w-0">
                <span class="text-xs text-muted-foreground font-mono">#{item.id}</span>
                {#if item.forward_attempts === 0}
                  <span class="inline-flex items-center rounded-full bg-green-100 dark:bg-green-900/30 px-2 py-0.5 text-xs font-medium text-green-700 dark:text-green-400">
                    new
                  </span>
                {:else}
                  <span class="inline-flex items-center rounded-full bg-purple-100 dark:bg-purple-900/30 px-2 py-0.5 text-xs font-medium text-purple-700 dark:text-purple-400">
                    {item.forward_attempts} attempt{item.forward_attempts !== 1 ? "s" : ""}
                  </span>
                {/if}
                {#if item.last_attempt_error}
                  <span class="inline-flex items-center rounded-full bg-red-100 dark:bg-red-900/30 px-2 py-0.5 text-xs font-medium text-red-700 dark:text-red-400">
                    error
                  </span>
                {/if}
              </div>
              <div class="flex items-center gap-2 shrink-0">
                <span class="text-xs text-muted-foreground hidden sm:inline">
                  {formatDate(item.received_at)}
                  {#if item.last_attempt_at}
                    &middot; last attempt {relativeTime(item.last_attempt_at)}
                  {/if}
                </span>
                <Button
                  variant="ghost"
                  size="sm"
                  onclick={(e: MouseEvent) => { e.stopPropagation(); handleRetry(item.id); }}
                  disabled={retrying === item.id}
                  title="Retry"
                  class="h-7 px-2"
                >
                  <RefreshCw class="w-3.5 h-3.5 {retrying === item.id ? 'animate-spin' : ''}" />
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  onclick={(e: MouseEvent) => { e.stopPropagation(); handleDelete(item.id); }}
                  title="Delete"
                  class="h-7 px-2 text-destructive hover:text-destructive hover:bg-destructive/10"
                >
                  <Trash2 class="w-3.5 h-3.5" />
                </Button>
              </div>
            </div>
          </div>

          {#if expanded.has(item.id)}
            <div class="border-t px-4 pb-4 pt-3 space-y-3">
              <div class="grid grid-cols-1 md:grid-cols-2 gap-3">
                <div>
                  <div class="text-xs font-medium text-muted-foreground mb-1">Headers</div>
                  <div class="text-xs font-mono bg-muted p-2 rounded overflow-x-auto max-h-48 overflow-y-auto">
                    {#each Object.entries(item.headers) as [key, value] (key)}
                      <div><span class="text-muted-foreground">{key}:</span> {value}</div>
                    {/each}
                  </div>
                </div>
                <div>
                  <div class="text-xs font-medium text-muted-foreground mb-1">Payload</div>
                  <pre class="text-xs font-mono bg-muted p-2 rounded overflow-x-auto max-h-48 overflow-y-auto">{JSON.stringify(item.payload, null, 2)}</pre>
                </div>
              </div>
              {#if item.last_attempt_error}
                <div>
                  <div class="text-xs font-medium text-destructive mb-1">Last error</div>
                  <div class="text-xs font-mono bg-destructive/10 text-destructive p-2 rounded overflow-x-auto">
                    {item.last_attempt_error}
                  </div>
                </div>
              {/if}
            </div>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</div>
