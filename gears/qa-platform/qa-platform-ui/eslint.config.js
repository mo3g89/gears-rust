// Flat config for ESLint 9. Built only from what package.json's devDependencies already
// install (@typescript-eslint/parser + @typescript-eslint/eslint-plugin, eslint-plugin-react-hooks,
// eslint-plugin-react-refresh, and eslint's own @eslint/js recommended config) -- the
// `typescript-eslint` meta-package is not installed and is intentionally not used here.
import js from "@eslint/js";
import tsPlugin from "@typescript-eslint/eslint-plugin";
import tsParser from "@typescript-eslint/parser";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";

export default [
  {
    ignores: ["dist/**", "node_modules/**", "src/api/generated/openapi.d.ts"],
  },
  js.configs.recommended,
  {
    files: ["**/*.{ts,tsx}"],
    languageOptions: {
      parser: tsParser,
      parserOptions: {
        ecmaFeatures: { jsx: true },
      },
    },
    plugins: {
      "@typescript-eslint": tsPlugin,
      "react-hooks": reactHooks,
      "react-refresh": reactRefresh,
    },
    rules: {
      ...tsPlugin.configs.recommended.rules,
      ...reactHooks.configs["recommended-latest"].rules,
      // Measured 21 warnings across 11 files, every one a component file that also
      // exports a small pure helper (or a hook, or a cva `*Variants` function) colocated
      // on purpose -- several of those helpers (missingRequired, cleanRunParameters,
      // buildBlocks, exclusivityToParam/FromValue, planLabelFromId, unavailableTitle,
      // normalizeNotificationsConfig, ...) are unit-tested directly by name from that
      // same file. This rule only protects Vite's dev-time Fast Refresh from an extra
      // remount; it doesn't affect production behaviour and doesn't suit how this
      // codebase organizes files, so it is off rather than fought file by file.
      "react-refresh/only-export-components": "off",
      // Base no-undef does not understand ambient/global type declarations (the DOM lib's
      // HTMLDivElement & co., the UMD `React` namespace from @types/react, Node's
      // __dirname/require in the root *.config.ts files) and flags all of them as
      // undefined. TypeScript's own checker already catches genuinely undefined
      // identifiers, more accurately than this rule can for a TS codebase -- this is the
      // documented typescript-eslint guidance, not a workaround for this codebase's code.
      "no-undef": "off",
    },
  },
  {
    linterOptions: {
      reportUnusedDisableDirectives: true,
    },
  },
];
