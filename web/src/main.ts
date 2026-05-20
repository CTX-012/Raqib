import './app.css';
import { mount } from 'svelte';
import App from './App.svelte';

// Sprint-6 — Svelte 5 mount API. Falls through to a console error
// rather than throwing if `#app` is missing, so a malformed
// `index.html` is recoverable for debugging.
const target = document.getElementById('app');
if (!target) {
    console.error('edge_monitor: missing #app mount point in index.html');
} else {
    mount(App, { target });
}
