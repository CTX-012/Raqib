/** @type {import('tailwindcss').Config} */
export default {
    content: ['./index.html', './src/**/*.{svelte,ts,js}'],
    darkMode: 'class',
    theme: {
        extend: {
            // Sprint-6 — theme colors mirror ux_contract::{DARK,
            // LIGHT, HIGH_CONTRAST} hex values from §13 so the
            // dashboard reads identically on web and TUI. Three
            // CSS classes (`.theme-dark`, `.theme-light`,
            // `.theme-hc`) carry the palette via CSS variables.
            colors: {
                fg: 'rgb(var(--em-fg) / <alpha-value>)',
                'fg-muted': 'rgb(var(--em-muted) / <alpha-value>)',
                accent: 'rgb(var(--em-accent) / <alpha-value>)',
                healthy: 'rgb(var(--em-healthy) / <alpha-value>)',
                attention: 'rgb(var(--em-attention) / <alpha-value>)',
                critical: 'rgb(var(--em-critical) / <alpha-value>)',
                bg: 'rgb(var(--em-bg) / <alpha-value>)',
                'bg-raised': 'rgb(var(--em-bg-raised) / <alpha-value>)',
            },
        },
    },
    plugins: [],
};
