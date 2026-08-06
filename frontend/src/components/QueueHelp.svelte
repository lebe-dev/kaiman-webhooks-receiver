<script lang="ts">
  import type { ChannelConfig } from "$lib/api";
  import { formatSeconds } from "$lib/format";
  import { ChevronDown, BookOpen } from "@lucide/svelte";

  let { config }: { config: ChannelConfig } = $props();

  let open = $state(false);

  let interval = $derived(formatSeconds(config.intervalSeconds ?? 0));
  let maxDelay = $derived(formatSeconds(config.backoff?.maxSeconds ?? 0));
</script>

<div class="rounded-lg border bg-muted/40">
  <button
    type="button"
    class="flex w-full items-center gap-2 px-4 py-2.5 text-left text-sm font-medium"
    onclick={() => (open = !open)}
    aria-expanded={open}
  >
    <BookOpen class="h-4 w-4 text-muted-foreground" />
    <span>How this queue is processed</span>
    <ChevronDown
      class="ml-auto h-4 w-4 text-muted-foreground transition-transform {open
        ? 'rotate-180'
        : ''}"
    />
  </button>

  {#if open}
    <ul class="space-y-2 border-t px-4 py-3 text-sm text-muted-foreground">
      <li>
        <span class="font-medium text-foreground">Oldest first, one at a time.</span>
        A webhook leaves the queue only when
        <span class="font-mono">{config.forwardUrl}</span>
        answers
        <span class="font-mono">{config.expectedStatus}</span>
        within {formatSeconds(config.timeoutSeconds ?? 0)}.
      </li>
      <li>
        <span class="font-medium text-foreground">Pace.</span>
        The forwarder looks at the queue every {interval}. After a successful delivery
        it moves straight to the next webhook, so a healthy queue drains without
        waiting out the interval.
      </li>
      <li>
        <span class="font-medium text-foreground">Failures back off.</span>
        Every failed attempt pushes that webhook's next attempt further out —
        starting at {interval}, multiplied by
        {config.backoff?.multiplier} each time, capped at {maxDelay} and spread by
        ±{Math.round((config.backoff?.jitter ?? 0) * 100)}%. The delay belongs to the
        webhook, not the channel, so a webhook the target keeps refusing does not hold
        up the ones behind it.
      </li>
      <li>
        <span class="font-medium text-foreground">Rejected payloads wait longest.</span>
        A <span class="font-mono">4xx</span> other than
        <span class="font-mono">408</span>/<span class="font-mono">429</span>
        jumps to the {maxDelay} delay immediately — retrying quickly cannot fix a
        payload the target refuses.
      </li>
      <li>
        <span class="font-medium text-foreground">Retry-After is honoured.</span>
        If the target answers with a <span class="font-mono">Retry-After</span> in
        seconds, that wins over the computed delay (still capped at {maxDelay}).
      </li>
      <li>
        <span class="font-medium text-foreground">Nothing is dropped.</span>
        Retries never stop, they only become rare. A webhook disappears only when it is
        delivered, deleted here, or the queue is cleared.
      </li>
      <li>
        <span class="font-medium text-foreground">Pausing</span>
        stops delivery attempts only. Incoming webhooks keep arriving and pile up in
        the queue until you resume.
      </li>
    </ul>
  {/if}
</div>
