<script lang="ts">
  import type { ChannelConfig } from "$lib/api";
  import { formatBytes, formatSeconds } from "$lib/format";
  import Hint from "./Hint.svelte";
  import { ChevronDown, Settings2, TriangleAlert } from "@lucide/svelte";

  let { config }: { config: ChannelConfig } = $props();

  let open = $state(false);

  // Kept out of the markup so the explanations stay readable and can hold line breaks.
  const HINT = {
    secretType:
      "How the sender authenticates a webhook.\nplain — the secret is compared as-is.\nhmac-sha256 — the sender signs the raw body and the proxy recomputes the signature.",
    secretHeader:
      "Request header the secret or signature is read from. Empty means the channel reads the secret from the body instead, according to its extract template.",
    hasSecret:
      "Whether a webhook-secret is set for this channel. The value never leaves the server, so it is not shown here.\nWithout a secret the channel accepts any request that reaches it.",
    maxBodySize:
      "Largest accepted payload. A bigger request is rejected with 413 and never reaches the queue.\nFalls back to the global default when the channel sets no limit of its own.",
    allowedIps:
      "Source addresses allowed to send to this channel (single IPs or CIDR ranges). Anything else is rejected with 403.\n“any” means the channel has no IP restriction.",
    metrics:
      "Whether this channel contributes to the /api/metrics counters. Disabling it hides the channel from monitoring, not from the log.",
    forwardUrl:
      "Where queued webhooks are POSTed. The original headers are forwarded, minus the ignored and hop-by-hop ones.",
    expectedStatus:
      "The one response status counted as delivered — only then is the webhook removed from the queue. Every other status is a failure and triggers a retry.",
    timeout:
      "How long a single delivery attempt may take before it is abandoned and treated as a transient failure.",
    interval:
      "Pause between passes over the queue, and the delay before the first retry.\nAfter a successful delivery the next webhook is sent immediately, without waiting out the interval.",
    backoff:
      "Each consecutive failure multiplies the delay of that webhook, up to the cap. The delay is stored per webhook, so a rejected one does not hold up the queue behind it.\nRetries never stop — they only become rare.",
    jitter:
      "Random spread applied to every delay, as a fraction of it. Keeps several channels or replicas from retrying in lockstep.",
    schedule:
      "Delays between the first attempts of one failing webhook, before jitter. The last value is the cap and repeats forever.",
    signHeader:
      "Header the outgoing HMAC-SHA256 signature of the body is written to. Empty means forwarded requests are not signed.",
    signKey:
      "Which secret signs the forwarded body. A channel without its own sign-secret reuses the webhook-secret. Only the source is shown, never the value.",
  };

  // Settings worth a second look. Each one states what the setting actually costs,
  // not just that it is unusual — the operator has to be able to decide from the text
  // whether it is deliberate.
  const WARN = {
    noSecret:
      "No webhook-secret: every request that reaches this channel is accepted and queued, whoever sent it. Only an IP allowlist or a network boundary keeps strangers out.",
    wideOpen:
      "Neither a secret nor an IP allowlist. Anyone who learns this URL can push webhooks into the channel — and, on a forwarding channel, into the target behind it.",
    metricsOff:
      "This channel is invisible to Prometheus: no received, forwarded or failed counters come out of it, so alerts built on those metrics can never fire for this channel.",
    plainHttp:
      "The target is plain HTTP. Payloads, forwarded headers and the signature travel unencrypted and can be read or altered in transit.",
    oddExpectedStatus:
      "Expected status is outside 2xx, so an ordinary success from the target counts as a failure and the webhook is retried forever. Only set this if the target really answers with this code.",
    hugeBody:
      "A body limit this large lets a single request hold that much memory while it is read and stored. Lower it to the biggest payload the sender actually produces.",
    noBackoffGrowth:
      "The multiplier is 1, so failures never slow anything down: a dead target keeps being hit on every interval, indefinitely.",
    noJitter:
      "Jitter is off. Retries of this channel line up exactly with the interval, and several replicas will hit the target at the same moment.",
    signWithoutSecret:
      "A sign-header is configured but there is neither a sign-secret nor a webhook-secret to sign with. Forwarding this channel fails with an internal error until one is set.",
    secretWithoutSignHeader:
      "A sign-secret is configured but no sign-header, so nothing is ever signed. The target receives unsigned requests.",
  };

  /** Warning attached to a value, or null when the setting needs no comment. */
  let warn = $derived({
    hasSecret: config.hasSecret ? null : WARN.noSecret,
    allowedIps:
      !config.allowedIps?.length && !config.hasSecret ? WARN.wideOpen : null,
    // 100 MB is the hard ceiling the config validator allows; half of it in a single
    // request is already worth questioning.
    maxBodySize: config.maxBodySize > 50 * 1024 * 1024 ? WARN.hugeBody : null,
    metrics: config.monitoringMetrics ? null : WARN.metricsOff,
    forwardUrl: config.forwardUrl?.startsWith("http://") ? WARN.plainHttp : null,
    expectedStatus:
      config.expectedStatus !== null &&
      (config.expectedStatus < 200 || config.expectedStatus >= 300)
        ? WARN.oddExpectedStatus
        : null,
    backoff:
      config.backoff && config.backoff.multiplier <= 1
        ? WARN.noBackoffGrowth
        : null,
    jitter: config.backoff && config.backoff.jitter <= 0 ? WARN.noJitter : null,
    signHeader: config.signHeader
      ? !config.hasSignSecret && !config.hasSecret
        ? WARN.signWithoutSecret
        : null
      : config.hasSignSecret
        ? WARN.secretWithoutSignHeader
        : null,
  });

  // Shown on the collapsed header: without it a warning inside a folded card would be
  // invisible, which is what the removed page-level banner used to cover.
  let warningCount = $derived(Object.values(warn).filter(Boolean).length);

  /** The first few retry delays, so the schedule is readable without doing the maths. */
  let backoffPreview = $derived.by(() => {
    if (!config.intervalSeconds || !config.backoff) return null;

    const { multiplier, maxSeconds } = config.backoff;
    // A multiplier of 1 disables growth, so listing the same value four times
    // would read as a schedule that goes somewhere.
    if (multiplier <= 1) {
      return `${formatSeconds(config.intervalSeconds)} (constant)`;
    }

    const delays: string[] = [];
    let delay = config.intervalSeconds;
    for (let i = 0; i < 4 && delay < maxSeconds; i++) {
      delays.push(formatSeconds(Math.round(delay)));
      delay = delay * multiplier;
    }
    delays.push(formatSeconds(maxSeconds));
    return delays.join(" → ");
  });
</script>

{#snippet row(
  label: string,
  hint: string,
  value: string,
  warning: string | null = null,
  mono = true,
)}
  <div class="flex items-baseline gap-2">
    <dt class="flex w-40 shrink-0 items-center gap-1 text-muted-foreground">
      {label}
      <Hint text={hint} />
    </dt>
    <dd
      class="flex items-center gap-1 break-all {mono ? 'font-mono' : ''} {warning
        ? 'text-amber-600 dark:text-amber-400'
        : ''}"
    >
      {value}
      {#if warning}
        <Hint
          text={warning}
          class="text-amber-600 hover:text-amber-800 dark:text-amber-400 dark:hover:text-amber-200"
        >
          <TriangleAlert class="h-3.5 w-3.5" />
        </Hint>
      {/if}
    </dd>
  </div>
{/snippet}

<div class="rounded-lg border bg-card text-card-foreground shadow-card">
  <button
    type="button"
    class="flex w-full items-center gap-2 px-4 py-3 text-left text-sm font-medium"
    onclick={() => (open = !open)}
    aria-expanded={open}
  >
    <Settings2 class="h-4 w-4 text-muted-foreground" />
    <span>Channel configuration</span>
    <span class="text-xs font-normal text-muted-foreground">
      {config.hasForward ? "forwarding" : "storage only"}
    </span>
    {#if warningCount > 0 && !open}
      <span
        class="inline-flex items-center gap-1 rounded-full bg-amber-500/10 px-2 py-0.5 text-xs font-medium text-amber-600 dark:text-amber-400"
      >
        <TriangleAlert class="h-3 w-3" />
        {warningCount}
      </span>
    {/if}
    <ChevronDown
      class="ml-auto h-4 w-4 text-muted-foreground transition-transform {open
        ? 'rotate-180'
        : ''}"
    />
  </button>

  {#if open}
    <div class="grid gap-6 border-t px-4 py-4 md:grid-cols-2">
      <section class="space-y-2">
        <h3 class="text-xs font-semibold tracking-wide text-muted-foreground uppercase">
          Receiving
        </h3>
        <dl class="space-y-1.5 text-sm">
          {@render row("Secret type", HINT.secretType, config.secretType)}
          {@render row(
            "Secret header",
            HINT.secretHeader,
            config.secretHeader ?? "—",
          )}
          {@render row(
            "Secret configured",
            HINT.hasSecret,
            config.hasSecret ? "yes" : "no — requests are not verified",
            warn.hasSecret,
            false,
          )}
          {@render row(
            "Max body size",
            HINT.maxBodySize,
            formatBytes(config.maxBodySize),
            warn.maxBodySize,
          )}
          {@render row(
            "Allowed IPs",
            HINT.allowedIps,
            config.allowedIps?.length ? config.allowedIps.join(", ") : "any",
            warn.allowedIps,
          )}
          {@render row(
            "Prometheus metrics",
            HINT.metrics,
            config.monitoringMetrics ? "enabled" : "disabled",
            warn.metrics,
            false,
          )}
        </dl>
      </section>

      <section class="space-y-2">
        <h3 class="text-xs font-semibold tracking-wide text-muted-foreground uppercase">
          Forwarding
        </h3>
        {#if !config.hasForward}
          <p class="text-sm text-muted-foreground">
            No forward target. Webhooks are stored until a client polls them from
            <span class="font-mono">/api/webhook/{config.name}</span>.
          </p>
        {:else}
          <dl class="space-y-1.5 text-sm">
            {@render row(
              "Target URL",
              HINT.forwardUrl,
              config.forwardUrl ?? "—",
              warn.forwardUrl,
            )}
            {@render row(
              "Expected status",
              HINT.expectedStatus,
              String(config.expectedStatus),
              warn.expectedStatus,
            )}
            {@render row(
              "Timeout",
              HINT.timeout,
              formatSeconds(config.timeoutSeconds ?? 0),
            )}
            {@render row(
              "Interval",
              HINT.interval,
              formatSeconds(config.intervalSeconds ?? 0),
            )}
            {#if config.backoff}
              {@render row(
                "Retry backoff",
                HINT.backoff,
                `×${config.backoff.multiplier}, max ${formatSeconds(config.backoff.maxSeconds)}`,
                warn.backoff,
              )}
              {@render row(
                "Jitter",
                HINT.jitter,
                `±${Math.round(config.backoff.jitter * 100)}%`,
                warn.jitter,
              )}
              {#if backoffPreview}
                {@render row("Schedule", HINT.schedule, backoffPreview)}
              {/if}
            {/if}
            {@render row(
              "Sign header",
              HINT.signHeader,
              config.signHeader ?? "—",
              warn.signHeader,
            )}
            {#if config.signHeader}
              {@render row(
                "Signing key",
                HINT.signKey,
                config.hasSignSecret
                  ? "dedicated sign-secret"
                  : "channel webhook-secret",
                null,
                false,
              )}
            {/if}
          </dl>
        {/if}
      </section>
    </div>
  {/if}
</div>
