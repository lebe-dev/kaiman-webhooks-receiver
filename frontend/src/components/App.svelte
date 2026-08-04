<script lang="ts">
    import TokenGate from "./TokenGate.svelte";
    import ChannelSelect from "./ChannelSelect.svelte";
    import WebhooksList from "./WebhooksList.svelte";
    import DebugSend from "./DebugSend.svelte";
    import QueueView from "./QueueView.svelte";
    import {
        Tabs,
        TabsList,
        TabsTrigger,
        TabsContent,
    } from "$lib/components/ui/tabs";
    import { Toaster } from "$lib/components/ui/sonner";
    import { ModeWatcher } from "mode-watcher";
    import { FishingHook, Bug, ListOrdered, LogOut, Info } from "@lucide/svelte";
    import { Button } from "$lib/components/ui/button/index.js";
    import { fetchConfig, type AppConfigResponse } from "$lib/api";
    import { clearToken } from "$lib/auth";
    import { version } from "../../package.json";
    import ThemeToggler from "./ThemeToggler.svelte";

    let config = $state<AppConfigResponse | null>(null);
    let selectedChannel = $state("");
    let configError = $state(false);
    let activeTab = $state("viewer");
    let debugPayload = $state("{\n  \n}");

    let currentChannelConfig = $derived(
        config?.channels.find((c) => c.name === selectedChannel) ?? null,
    );

    let currentChannelHasForward = $derived(
        currentChannelConfig?.hasForward ?? false,
    );

    $effect(() => {
        if (activeTab === "queue" && !currentChannelHasForward) {
            activeTab = "viewer";
        }
    });

    const CHANNEL_STORAGE_KEY = "kwp:selectedChannel";

    async function loadConfig() {
        try {
            config = await fetchConfig();
            if (config.channels.length > 0) {
                const cached = localStorage.getItem(CHANNEL_STORAGE_KEY);
                if (cached && config.channels.some((c) => c.name === cached)) {
                    selectedChannel = cached;
                } else {
                    localStorage.removeItem(CHANNEL_STORAGE_KEY);
                    selectedChannel = config.channels[0].name;
                }
            }
        } catch {
            configError = true;
        }
    }

    $effect(() => {
        if (selectedChannel) {
            localStorage.setItem(CHANNEL_STORAGE_KEY, selectedChannel);
        }
    });

    function logout() {
        clearToken();
        window.location.reload();
    }

    function copyToDebug(payload: string) {
        debugPayload = payload;
        activeTab = "debug";
    }
</script>

<ModeWatcher />
<Toaster />
<TokenGate>
    <div class="p-6 max-w-5xl mx-auto">
            <div class="flex items-center justify-between mb-6">
                <h1 class="text-xl font-semibold">
                    <div
                        class="text-primary dark:text-primary/70 text-sm font-medium"
                    >
                        Kaiman
                    </div>
                    Webhooks Proxy
                </h1>

                <div class="flex items-center gap-2">
                    <ThemeToggler></ThemeToggler>

                    <Button
                        onclick={logout}
                        variant="ghost"
                        size="icon"
                        title="Logout"
                    >
                        <LogOut class="w-4 h-4" />
                    </Button>
                </div>
            </div>

            {#if config === null && !configError}
                {#await loadConfig()}
                    <p class="text-sm text-muted-foreground">
                        Loading configuration...
                    </p>
                {/await}
            {:else if configError}
                <p class="text-sm text-destructive">
                    Failed to load configuration.
                </p>
            {:else if config}
                <div class="mb-4">
                    <label class="text-sm font-medium mr-2" for="channel-select"
                        >Channel:</label
                    >
                    <ChannelSelect
                        channels={config.channels}
                        bind:selected={selectedChannel}
                    />
                </div>

                {#if currentChannelConfig?.note}
                    <div
                        class="mb-4 flex gap-2 rounded-lg border border-border bg-muted p-3 text-sm text-muted-foreground"
                    >
                        <Info class="w-4 h-4 mt-0.5 shrink-0" />
                        <span class="whitespace-pre-wrap">{currentChannelConfig.note}</span>
                    </div>
                {/if}

                {#if currentChannelConfig && !currentChannelConfig.monitoringMetrics}
                    <div
                        class="mb-4 rounded-lg border border-amber-500/50 bg-amber-500/10 p-3 text-sm text-amber-700 dark:text-amber-400"
                    >
                        Prometheus metrics are disabled for this channel.
                    </div>
                {/if}

                <Tabs bind:value={activeTab}>
                    <TabsList>
                        <TabsTrigger value="viewer">
                            <FishingHook class="w-4 h-4" />
                            Webhooks
                        </TabsTrigger>
                        <TabsTrigger value="debug">
                            <Bug class="w-4 h-4" />
                            Debug
                        </TabsTrigger>
                        {#if currentChannelHasForward}
                            <TabsTrigger value="queue">
                                <ListOrdered class="w-4 h-4" />
                                Queue
                            </TabsTrigger>
                        {/if}
                    </TabsList>
                    <TabsContent value="viewer">
                        {#if selectedChannel}
                            <WebhooksList
                                channel={selectedChannel}
                                onCopyToDebug={copyToDebug}
                            />
                        {/if}
                    </TabsContent>
                    <TabsContent value="debug">
                        {#if selectedChannel && currentChannelConfig}
                            <DebugSend
                                channel={selectedChannel}
                                channelConfig={currentChannelConfig}
                                bind:payloadInput={debugPayload}
                            />
                        {/if}
                    </TabsContent>
                    <TabsContent value="queue">
                        {#if selectedChannel && currentChannelHasForward}
                            <QueueView channel={selectedChannel} />
                        {/if}
                    </TabsContent>
                </Tabs>
            {/if}

            <footer
                class="mt-12 pt-6 border-t text-xs text-muted-foreground flex items-center justify-center gap-2"
            >
                <span>v{version}</span>
                <span>|</span>
                <a
                    href="https://github.com/lebe-dev/kaiman-webhooks-proxy"
                    target="_blank"
                    rel="noopener noreferrer"
                    class="underline hover:text-foreground transition-colors"
                >
                    GITHUB
                </a>
            </footer>
        </div>
</TokenGate>
