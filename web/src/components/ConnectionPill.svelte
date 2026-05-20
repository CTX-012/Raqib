<script lang="ts">
    import type { ConnectionStatus } from '../lib/stores';
    export let status: ConnectionStatus;

    const LABEL: Record<ConnectionStatus, string> = {
        connecting: 'connecting…',
        connected: 'live',
        disconnected: 'offline',
    };
    const COLOR: Record<ConnectionStatus, string> = {
        connecting: 'text-attention',
        connected: 'text-healthy',
        disconnected: 'text-critical',
    };
</script>

<span class="text-xs {COLOR[status]}">● {LABEL[status]}</span>
