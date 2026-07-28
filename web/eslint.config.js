import js from '@eslint/js'
import globals from 'globals'
import reactHooks from 'eslint-plugin-react-hooks'
import reactRefresh from 'eslint-plugin-react-refresh'
import tseslint from 'typescript-eslint'
import { defineConfig, globalIgnores } from 'eslint/config'

export default defineConfig([
  globalIgnores(['dist']),
  {
    files: ['**/*.{ts,tsx}'],
    extends: [
      js.configs.recommended,
      tseslint.configs.recommended,
      reactHooks.configs.flat.recommended,
      reactRefresh.configs.vite,
    ],
    languageOptions: {
      globals: globals.browser,
    },
  },
  {
    // shadcn-generated primitives (not hand-authored, not touched here) -
    // both files co-export small helper functions (`badgeVariants`,
    // `buttonVariants`) alongside their component, which is the shadcn
    // convention upstream and not worth diverging from. That trips
    // react-refresh/only-export-components because Vite's Fast Refresh
    // wants component-only modules; it's a dev-experience lint, not a
    // correctness one, so it's scoped off for just this directory rather
    // than suppressed globally or left failing CI.
    files: ['src/components/ui/**/*.{ts,tsx}'],
    rules: {
      'react-refresh/only-export-components': 'off',
    },
  },
])
