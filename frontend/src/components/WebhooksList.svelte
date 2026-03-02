<script lang="ts">
  import { deleteWebhook, listWebhooks, type WebhookItem } from "$lib/api";
  import { Button } from "$lib/components/ui/button";
  import { toast } from "svelte-sonner";
  import { RotateCw, Copy, Trash2 } from "@lucide/svelte";

  let { channel, onCopyToDebug }: { channel: string; onCopyToDebug?: (payload: string) => void } = $props();

  let webhooks = $state<WebhookItem[]>([]);
  let loading = $state(false);
  let expandedHeaders = $state<Set<number>>(new Set());
  let deleteConfirmId = $state<number | null>(null);
  let deleting = $state(false);

  async function load() {
    loading = true;
    try {
      webhooks = await listWebhooks(channel);
    } catch {
      toast.error("Failed to load webhooks");
    } finally {
      loading = false;
    }
  }

  function toggleHeaders(id: number) {
    const next = new Set(expandedHeaders);
    if (next.has(id)) {
      next.delete(id);
    } else {
      next.add(id);
    }
    expandedHeaders = next;
  }

  function formatDate(ts: number): string {
    return new Date(ts * 1000).toLocaleString();
  }

  function handleCopyToDebug(webhook: WebhookItem) {
    const payload = JSON.stringify(webhook.payload, null, 2);
    onCopyToDebug?.(payload);
    toast.success("Payload copied to Debug tab");
  }

  async function handleDelete(id: number) {
    if (!confirm(`Delete webhook #${id}?`)) {
      return;
    }

    deleting = true;
    try {
      await deleteWebhook(channel, id);
      webhooks = webhooks.filter((w) => w.id !== id);
      toast.success("Webhook deleted");
      deleteConfirmId = null;
    } catch {
      toast.error("Failed to delete webhook");
    } finally {
      deleting = false;
    }
  }

  $effect(() => {
    channel;
    expandedHeaders = new Set();
    load();
  });
</script>

<div class="space-y-4">
  <div class="flex items-center gap-2">
    <Button
      variant="outline"
      size="sm"
      onclick={load}
      disabled={loading}
      class={loading ? "animate-spin" : ""}
    >
      <RotateCw size={16} />
    </Button>
    <span class="text-sm text-muted-foreground">
      {webhooks.length} webhook{webhooks.length !== 1 ? "s" : ""}
    </span>
  </div>

  {#if webhooks.length === 0 && !loading}
    <p class="text-sm text-muted-foreground">No webhooks stored.</p>
  {:else}
    <div class="space-y-3">
      {#each webhooks as wh (wh.id)}
        <div class="rounded-lg border p-4 space-y-2">
          <div class="flex items-center justify-between">
            <span class="text-xs text-muted-foreground font-mono">
              #{wh.id} &middot; {formatDate(wh.received_at)}
            </span>
            <div class="flex items-center gap-2">
              <button
                class="text-xs text-muted-foreground hover:text-foreground underline"
                onclick={() => toggleHeaders(wh.id)}
              >
                {expandedHeaders.has(wh.id) ? "hide headers" : "show headers"}
              </button>
              <Button
                variant="ghost"
                size="sm"
                onclick={() => handleCopyToDebug(wh)}
                title="Copy to Debug tab"
                class="h-5 px-2"
              >
                <Copy class="w-3 h-3" />
              </Button>
              <Button
                variant="ghost"
                size="sm"
                onclick={() => handleDelete(wh.id)}
                disabled={deleting}
                title="Delete webhook"
                class="h-5 px-2 text-destructive hover:text-destructive hover:bg-destructive/10"
              >
                <Trash2 class="w-3 h-3" />
              </Button>
            </div>
          </div>

          {#if expandedHeaders.has(wh.id)}
            <div class="text-xs font-mono bg-muted p-2 rounded overflow-x-auto">
              {#each Object.entries(wh.headers) as [key, value]}
                <div><span class="text-muted-foreground">{key}:</span> {value}</div>
              {/each}
            </div>
          {/if}

          <pre class="text-xs font-mono bg-muted p-2 rounded overflow-x-auto max-h-64 overflow-y-auto">{JSON.stringify(wh.payload, null, 2)}</pre>
        </div>
      {/each}
    </div>
  {/if}
</div>
