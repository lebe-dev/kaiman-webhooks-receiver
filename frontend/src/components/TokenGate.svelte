<script lang="ts">
    import type { Snippet } from "svelte";
    import { getToken, setToken } from "$lib/auth";
    import { Input } from "$lib/components/ui/input";
    import { Button } from "$lib/components/ui/button";
    import { toast } from "svelte-sonner";

    let { children }: { children: Snippet } = $props();

    let authenticated = $state(!!getToken());
    let tokenInput = $state("");
    let loading = $state(false);

    async function connect() {
        if (!tokenInput.trim()) return;
        loading = true;
        try {
            const res = await fetch("/api/config", {
                headers: { Authorization: `Bearer ${tokenInput}` },
            });
            if (res.ok) {
                setToken(tokenInput);
                authenticated = true;
            } else {
                toast.error("Invalid token");
            }
        } catch {
            toast.error("Connection failed");
        } finally {
            loading = false;
        }
    }

    function handleKeydown(e: KeyboardEvent) {
        if (e.key === "Enter") connect();
    }
</script>

{#if authenticated}
    {@render children()}
{:else}
    <div class="flex min-h-screen items-center justify-center">
        <div class="w-full max-w-sm space-y-4 p-6">
            <h1 class="text-2xl font-bold text-center">
                <div class="text-gray-500 inline-block">Kaiman</div>
                Webhooks Proxy
            </h1>
            <p class="text-sm text-muted-foreground text-center">
                Enter your access token to continue
            </p>
            <Input
                type="password"
                placeholder="Access Token"
                bind:value={tokenInput}
                onkeydown={handleKeydown}
            />
            <Button class="w-full" onclick={connect} disabled={loading}>
                {loading ? "Connecting..." : "Login"}
            </Button>
        </div>
    </div>
{/if}
