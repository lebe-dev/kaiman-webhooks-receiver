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
    type ChannelConfig,
    type ChannelForwardStatus,
  } from "$lib/api";
  import { buttonVariants } from "$lib/components/ui/button";
  import { Input } from "$lib/components/ui/input";
  import {
    AlertDialog,
    AlertDialogAction,
    AlertDialogCancel,
    AlertDialogContent,
    AlertDialogDescription,
    AlertDialogFooter,
    AlertDialogHeader,
    AlertDialogTitle,
  } from "$lib/components/ui/alert-dialog";
  import { toast } from "svelte-sonner";
  import { RotateCw, Pause, Play, Trash2, RefreshCw } from "@lucide/svelte";
  import Hint from "./Hint.svelte";
  import QueueHelp from "./QueueHelp.svelte";
  import { formatSeconds } from "$lib/format";

  let {
    channel,
    channelConfig,
  }: { channel: string; channelConfig: ChannelConfig } = $props();

  let items = $state<QueueItem[]>([]);
  let status = $state<ChannelForwardStatus>({
    paused: false,
    queue_size: 0,
    last_success_at: null,
    last_error_at: null,
    last_error_message: null,
    next_attempt_at: null,
  });
  let loading = $state(false);
  let expanded = new SvelteSet<number>();
  let retrying = $state<number | null>(null);
  // Countdowns are derived from this rather than from Date.now() directly, so they
  // keep ticking even when a poll brings back an unchanged queue.
  let nowTs = $state(Math.floor(Date.now() / 1000));

  let lastItemsJson = "";

  // Kept out of the markup so the explanations stay readable and can hold line breaks.
  const HINT = {
    queueSize:
      "Webhooks stored for this channel and not yet delivered — including the ones waiting out a retry delay.",
    status:
      "Active — the forwarder is delivering, or will on its next pass.\nBacking off — every queued webhook is waiting out a retry delay; nothing is sent until the soonest one is due.\nPaused — delivery is stopped by hand; incoming webhooks still pile up.",
    lastSuccess:
      "When this channel last got the expected status from the target. Resets to “never” on restart — it is not read back from the database.",
    lastError:
      "The most recent failed attempt on this channel, whichever webhook it was. Per-webhook errors are on the item itself.",
    refresh:
      "Reload the queue now. It also refreshes on its own every 5 seconds.",
    pause:
      "Stop delivery attempts for this channel. Incoming webhooks keep arriving and wait in the queue. The state is held in memory only — a restart resumes forwarding.",
    resume:
      "Resume delivery. Webhooks that arrived while paused are sent oldest first.",
    clear:
      "Permanently delete every queued webhook for this channel. They are never delivered. Asks for the channel name first.",
    clearEmpty: "The queue is already empty.",
    isNew: "Never sent yet — it goes out on the forwarder's next pass.",
    lastAttemptFailed:
      "The last attempt failed. Open the item to read the response the target gave.",
    waiting:
      "This webhook is waiting out its backoff and is skipped until then. Others in the queue are still delivered meanwhile.\nRetry now sends it immediately, ignoring the delay.",
    retryNow:
      "Send this webhook right now, ignoring its retry delay. On success it leaves the queue; on failure the delay is extended as usual.",
    deleteItem:
      "Drop this webhook without delivering it. This cannot be undone.",
  };

  let maxDelayLabel = $derived(
    channelConfig.backoff
      ? formatSeconds(channelConfig.backoff.maxSeconds)
      : "the cap",
  );

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

  /** Time left until a backoff delay expires, or null once it already has. */
  function timeUntil(ts: number | null): string | null {
    if (ts === null) return null;
    const diff = ts - nowTs;
    if (diff <= 0) return null;
    if (diff < 60) return `${diff}s`;
    if (diff < 3600) return `${Math.floor(diff / 60)}m ${diff % 60}s`;
    return `${Math.floor(diff / 3600)}h ${Math.floor((diff % 3600) / 60)}m`;
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

  // Clearing deletes undelivered webhooks for good, so it asks for the channel name
  // rather than a yes/no the operator can click through by reflex.
  let clearDialogOpen = $state(false);
  let clearConfirmation = $state("");
  let clearing = $state(false);
  let clearConfirmed = $derived(clearConfirmation.trim() === channel);

  function openClearDialog() {
    clearConfirmation = "";
    clearDialogOpen = true;
  }

  async function handleClear() {
    if (!clearConfirmed) return;
    clearing = true;
    const deleted = items.length;
    try {
      await clearQueue(channel);
      items = [];
      clearDialogOpen = false;
      toast.success(
        `Queue cleared — ${deleted} webhook${deleted !== 1 ? "s" : ""} deleted`,
      );
      await load();
    } catch {
      toast.error("Failed to clear queue");
    } finally {
      clearing = false;
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
    const interval = setInterval(() => {
      nowTs = Math.floor(Date.now() / 1000);
      load(true);
    }, 5000);
    return () => clearInterval(interval);
  });
</script>

<div class="space-y-4">
  <QueueHelp config={channelConfig} />

  <!-- Status panel -->
  <div class="grid grid-cols-2 md:grid-cols-4 gap-3">
    <div class="rounded-lg border bg-card text-card-foreground shadow-card p-3 space-y-1">
      <div class="flex items-center gap-1 text-xs text-muted-foreground">
        Queue size
        <Hint
          text={HINT.queueSize}
        />
      </div>
      <div class="text-2xl font-semibold">{status.queue_size}</div>
    </div>
    <div class="rounded-lg border bg-card text-card-foreground shadow-card p-3 space-y-1">
      <div class="flex items-center gap-1 text-xs text-muted-foreground">
        Status
        <Hint
          text={HINT.status}
        />
      </div>
      <div class="text-sm font-medium">
        {#if status.paused}
          <span class="inline-flex items-center gap-1.5 text-yellow-600 dark:text-yellow-400">
            <span class="w-2 h-2 rounded-full bg-yellow-500"></span>
            Paused
          </span>
        {:else if timeUntil(status.next_attempt_at)}
          <span class="inline-flex items-center gap-1.5 text-amber-600 dark:text-amber-400">
            <span class="w-2 h-2 rounded-full bg-amber-500"></span>
            Backing off
          </span>
        {:else}
          <span class="inline-flex items-center gap-1.5 text-green-600 dark:text-green-400">
            <span class="w-2 h-2 rounded-full bg-green-500"></span>
            Active
          </span>
        {/if}
      </div>
      {#if !status.paused && timeUntil(status.next_attempt_at)}
        <div class="text-xs text-muted-foreground">
          retry in {timeUntil(status.next_attempt_at)}
        </div>
      {/if}
    </div>
    <div class="rounded-lg border bg-card text-card-foreground shadow-card p-3 space-y-1">
      <div class="flex items-center gap-1 text-xs text-muted-foreground">
        Last success
        <Hint
          text={HINT.lastSuccess}
        />
      </div>
      <div class="text-sm font-medium">{relativeTime(status.last_success_at)}</div>
    </div>
    <div class="rounded-lg border bg-card text-card-foreground shadow-card p-3 space-y-1">
      <div class="flex items-center gap-1 text-xs text-muted-foreground">
        Last error
        <Hint
          text={HINT.lastError}
        />
      </div>
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
    <Hint
      text={HINT.refresh}
      onclick={() => load()}
      disabled={loading}
      class={buttonVariants({ variant: "outline", size: "sm" })}
    >
      <RotateCw size={16} class={loading ? "animate-spin" : ""} />
    </Hint>
    <Hint
      text={status.paused ? HINT.resume : HINT.pause}
      onclick={handlePauseResume}
      class={buttonVariants({ variant: "outline", size: "sm" })}
    >
      {#if status.paused}
        <Play size={16} />
        <span class="ml-1">Resume</span>
      {:else}
        <Pause size={16} />
        <span class="ml-1">Pause</span>
      {/if}
    </Hint>
    <Hint
      text={items.length === 0 ? HINT.clearEmpty : HINT.clear}
      onclick={openClearDialog}
      disabled={items.length === 0}
      class={buttonVariants({ variant: "destructive", size: "sm" })}
    >
      <Trash2 size={16} />
      <span class="ml-1">Clear Queue…</span>
    </Hint>
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
        <div class="rounded-lg border bg-card text-card-foreground shadow-card">
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
                  <Hint text={HINT.isNew}>
                    <span class="inline-flex items-center rounded-full bg-green-100 dark:bg-green-900/30 px-2 py-0.5 text-xs font-medium text-green-700 dark:text-green-400">
                      new
                    </span>
                  </Hint>
                {:else}
                  <Hint
                    text={`Failed delivery attempts so far. Each one lengthens this webhook's own retry delay, up to ${maxDelayLabel}.`}
                  >
                    <span class="inline-flex items-center rounded-full bg-purple-100 dark:bg-purple-900/30 px-2 py-0.5 text-xs font-medium text-purple-700 dark:text-purple-400">
                      {item.forward_attempts} attempt{item.forward_attempts !== 1 ? "s" : ""}
                    </span>
                  </Hint>
                {/if}
                {#if item.last_attempt_error}
                  <Hint text={HINT.lastAttemptFailed}>
                    <span class="inline-flex items-center rounded-full bg-red-100 dark:bg-red-900/30 px-2 py-0.5 text-xs font-medium text-red-700 dark:text-red-400">
                      error
                    </span>
                  </Hint>
                {/if}
                {#if timeUntil(item.next_attempt_at)}
                  <Hint
                    text={HINT.waiting}
                  >
                    <span class="inline-flex items-center rounded-full bg-amber-100 dark:bg-amber-900/30 px-2 py-0.5 text-xs font-medium text-amber-700 dark:text-amber-400">
                      retry in {timeUntil(item.next_attempt_at)}
                    </span>
                  </Hint>
                {/if}
              </div>
              <div class="flex items-center gap-2 shrink-0">
                <span class="text-xs text-muted-foreground hidden sm:inline">
                  {formatDate(item.received_at)}
                  {#if item.last_attempt_at}
                    &middot; last attempt {relativeTime(item.last_attempt_at)}
                  {/if}
                </span>
                <Hint
                  text={HINT.retryNow}
                  onclick={(e: MouseEvent) => { e.stopPropagation(); handleRetry(item.id); }}
                  disabled={retrying === item.id}
                  class="{buttonVariants({ variant: 'ghost', size: 'sm' })} h-7 px-2"
                >
                  <RefreshCw class="w-3.5 h-3.5 {retrying === item.id ? 'animate-spin' : ''}" />
                </Hint>
                <Hint
                  text={HINT.deleteItem}
                  onclick={(e: MouseEvent) => { e.stopPropagation(); handleDelete(item.id); }}
                  class="{buttonVariants({ variant: 'ghost', size: 'sm' })} h-7 px-2 text-destructive hover:text-destructive hover:bg-destructive/10"
                >
                  <Trash2 class="w-3.5 h-3.5" />
                </Hint>
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

<AlertDialog bind:open={clearDialogOpen}>
  <AlertDialogContent class="max-w-md sm:max-w-md">
    <AlertDialogHeader>
      <AlertDialogTitle>Clear the queue of “{channel}”?</AlertDialogTitle>
      <AlertDialogDescription>
        {items.length} queued webhook{items.length !== 1 ? "s" : ""} will be deleted
        from the database and never delivered to
        <span class="font-mono">{channelConfig.forwardUrl}</span>. Payloads are not
        kept anywhere else — this cannot be undone.
      </AlertDialogDescription>
    </AlertDialogHeader>

    <div class="space-y-2 py-2">
      <label class="text-sm text-muted-foreground" for="clear-confirm">
        Type <span class="font-mono font-medium text-foreground">{channel}</span> to confirm
      </label>
      <Input
        id="clear-confirm"
        bind:value={clearConfirmation}
        autocomplete="off"
        spellcheck={false}
        placeholder={channel}
      />
    </div>

    <AlertDialogFooter>
      <AlertDialogCancel>Cancel</AlertDialogCancel>
      <AlertDialogAction
        variant="destructive"
        disabled={!clearConfirmed || clearing}
        onclick={(e: MouseEvent) => {
          e.preventDefault();
          handleClear();
        }}
      >
        {clearing ? "Deleting…" : `Delete ${items.length} webhook${items.length !== 1 ? "s" : ""}`}
      </AlertDialogAction>
    </AlertDialogFooter>
  </AlertDialogContent>
</AlertDialog>
