import type { Config } from 'tailwindcss'

// Vendored from the embers-theme skill. Every colour maps to a CSS variable
// defined in src/styles/tokens.css; toggling class="dark" on <html> swaps the
// whole palette in one paint. The neutral source of truth for those values is
// design/tokens.json (repo root) — tokens.parity.spec.ts guards they agree.
export default {
  content: ['./index.html', './src/**/*.{vue,ts}'],
  darkMode: 'class',
  theme: {
    extend: {
      colors: {
        accent: 'var(--accent)',
        'accent-2': 'var(--accent-2)',
        'accent-soft': 'var(--accent-soft)',
        'accent-ring': 'var(--accent-ring)',

        surface: 'var(--surface)',
        'surface-2': 'var(--surface-2)',
        'surface-muted': 'var(--surface-muted)',

        hairline: 'var(--hairline)',
        'hairline-strong': 'var(--hairline-strong)',

        'text-primary': 'var(--text-primary)',
        'text-muted': 'var(--text-muted)',
        'text-subtle': 'var(--text-subtle)',

        ok: 'var(--ok)',
        warn: 'var(--warn)',
        danger: 'var(--danger)',
        'ok-soft': 'var(--ok-soft)',
        'warn-soft': 'var(--warn-soft)',
        'danger-soft': 'var(--danger-soft)',

        brand: {
          DEFAULT: 'var(--accent)',
          dark: 'var(--accent-2)',
          light: 'var(--accent-soft)',
        },
      },
      fontFamily: {
        sans: ['Inter', 'ui-sans-serif', 'system-ui', 'sans-serif'],
        mono: ['"JetBrains Mono"', 'ui-monospace', 'SFMono-Regular', 'monospace'],
      },
      borderRadius: {
        sm: 'var(--radius-sm)',
        md: 'var(--radius-md)',
        lg: 'var(--radius-lg)',
      },
      boxShadow: {
        card: 'var(--shadow-card)',
        cta: 'var(--shadow-cta)',
      },
      backdropBlur: {
        card: 'var(--blur-card)',
      },
      backgroundImage: {
        'embers-glow':
          'radial-gradient(1200px 600px at 80% -10%, var(--bg-glow-1), transparent 60%), radial-gradient(900px 500px at -10% 110%, var(--bg-glow-2), transparent 70%)',
        'accent-gradient': 'linear-gradient(135deg, var(--accent), var(--accent-2))',
      },
    },
  },
  plugins: [],
} satisfies Config
