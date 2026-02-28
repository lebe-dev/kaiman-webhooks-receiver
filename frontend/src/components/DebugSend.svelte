<script lang="ts">
  import { testSend, type ChannelConfig, type TestSendResult } from "$lib/api";
  import { Button } from "$lib/components/ui/button";
  import { Input } from "$lib/components/ui/input";
  import { Copy, Check } from "@lucide/svelte";
  import { toast } from "svelte-sonner";

  let {
    channel,
    channelConfig,
    payloadInput = $bindable('{\n  \n}'),
  }: {
    channel: string;
    channelConfig: ChannelConfig;
    payloadInput?: string;
  } = $props();
  let secretInput = $state("");
  let result = $state<TestSendResult | null>(null);
  let sending = $state(false);
  let parseError = $state<string | null>(null);
  let copyClicked = $state(false);

  const currentUnixTime = $derived.by(() => {
    return Math.floor(Date.now() / 1000);
  });

  function copyUnixTime() {
    navigator.clipboard.writeText(currentUnixTime.toString());
    copyClicked = true;
    setTimeout(() => {
      copyClicked = false;
    }, 2000);
  }

  function validatePayload(): unknown | null {
    try {
      const parsed = JSON.parse(payloadInput);
      parseError = null;
      return parsed;
    } catch (e) {
      parseError = (e as Error).message;
      return null;
    }
  }

  async function send() {
    const payload = validatePayload();
    if (payload === null) return;
    if (!secretInput.trim()) {
      toast.error("Secret is required");
      return;
    }
    sending = true;
    result = null;
    try {
      result = await testSend(channel, payload, secretInput);
    } catch (e) {
      toast.error((e as Error).message);
    } finally {
      sending = false;
    }
  }
</script>

<div class="space-y-4 max-w-2xl">
  <div class="space-y-2">
    <label class="text-sm font-medium">Current time (Unix timestamp)</label>
    <div class="flex items-center gap-2">
      <div class="flex-1 rounded-md border border-input bg-muted px-3 py-2 font-mono text-sm">
        {currentUnixTime}
      </div>
      <button
        onclick={copyUnixTime}
        class="inline-flex items-center justify-center rounded-md border border-input bg-transparent px-3 py-2 text-sm font-medium hover:bg-muted transition-colors"
        title="Copy to clipboard"
      >
        {#if copyClicked}
          <Check class="w-4 h-4 text-green-600" />
        {:else}
          <Copy class="w-4 h-4" />
        {/if}
      </button>
    </div>
  </div>

  {#if !channelConfig.hasForward}
    <div class="rounded-lg border border-destructive/50 bg-destructive/10 p-4 text-sm">
      This channel has no forward URL configured. Test send is not available.
    </div>
  {:else}
    <div class="text-sm text-muted-foreground space-y-1">
      <div>Forward URL: <span class="font-mono">{channelConfig.forwardUrl}</span></div>
      {#if channelConfig.signHeader}
        <div>Sign header: <span class="font-mono">{channelConfig.signHeader}</span></div>
      {/if}
      <div>Expected status: <span class="font-mono">{channelConfig.expectedStatus}</span></div>
    </div>

    <div class="space-y-2">
      <label class="text-sm font-medium" for="secret-input">Secret</label>
      <Input
        id="secret-input"
        type="password"
        placeholder="Webhook signing secret"
        bind:value={secretInput}
      />
    </div>

    <div class="space-y-2">
      <label class="text-sm font-medium" for="payload-input">Payload (JSON)</label>
      <textarea
        id="payload-input"
        bind:value={payloadInput}
        class="flex min-h-32 w-full rounded-md border border-input bg-transparent px-3 py-2 text-sm font-mono shadow-sm placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
        rows="8"
      ></textarea>
      {#if parseError}
        <p class="text-xs text-destructive">{parseError}</p>
      {/if}
    </div>

    <Button onclick={send} disabled={sending}>
      {sending ? "Sending..." : "Send Test Webhook"}
    </Button>

    {#if result}
      <div class="rounded-lg border p-4 space-y-2">
        <div class="flex items-center gap-2">
          <span class="text-sm font-medium">Response:</span>
          <span
            class="inline-flex items-center rounded-full px-2 py-0.5 text-xs font-medium {result.status === channelConfig.expectedStatus
              ? 'bg-green-100 text-green-800 dark:bg-green-900 dark:text-green-200'
              : 'bg-red-100 text-red-800 dark:bg-red-900 dark:text-red-200'}"
          >
            {result.status}
          </span>
        </div>
        <pre class="text-xs font-mono bg-muted p-2 rounded overflow-x-auto max-h-64 overflow-y-auto">{result.body}</pre>
      </div>
    {/if}
  {/if}
</div>
