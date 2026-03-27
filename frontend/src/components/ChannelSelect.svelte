<script lang="ts">
  import type { ChannelConfig } from "$lib/api";

  let {
    channels,
    selected = $bindable(),
  }: {
    channels: ChannelConfig[];
    selected: string;
  } = $props();
</script>

<select
  bind:value={selected}
  class="flex h-9 w-full max-w-xs rounded-md border border-input bg-transparent px-3 py-1 text-sm shadow-sm transition-colors focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
>
  {#each channels as ch (ch.name)}
    <option value={ch.name}>{ch.name}</option>
  {/each}
</select>
