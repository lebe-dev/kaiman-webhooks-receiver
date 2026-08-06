<script lang="ts">
  import {
    Tooltip,
    TooltipTrigger,
    TooltipContent,
  } from "$lib/components/ui/tooltip";
  import { Info } from "@lucide/svelte";
  import { cn } from "$lib/utils";
  import type { Snippet } from "svelte";

  let {
    text,
    side = "top",
    children,
    onclick,
    disabled = false,
    class: className,
  }: {
    /** Explanation shown on hover/focus. Line breaks are preserved. */
    text: string;
    side?: "top" | "bottom" | "left" | "right";
    /** Custom trigger. Without it the hint renders as a small info icon. */
    children?: Snippet;
    /** Makes the trigger act as the control it describes, instead of a bare hint. */
    onclick?: (event: MouseEvent) => void;
    disabled?: boolean;
    class?: string;
  } = $props();
</script>

<Tooltip>
  <TooltipTrigger
    type="button"
    {onclick}
    {disabled}
    class={cn(
      "inline-flex items-center rounded-sm align-middle transition-colors focus-visible:ring-1 focus-visible:ring-ring focus-visible:outline-none",
      onclick
        ? "disabled:pointer-events-none disabled:opacity-50"
        : "cursor-help text-muted-foreground hover:text-foreground",
      className,
    )}
  >
    {#if children}
      {@render children()}
    {:else}
      <Info class="h-3.5 w-3.5" />
    {/if}
  </TooltipTrigger>
  <TooltipContent {side} class="max-w-xs whitespace-pre-line text-xs leading-relaxed">
    {text}
  </TooltipContent>
</Tooltip>
