# Contributing translations to voxix

voxix is translated with [i18next](https://i18next.com/). All interface strings live in JSON files, one directory per locale:

```
ui/i18n/
├── index.ts               # i18n bootstrap
├── languages.ts           # language metadata
└── locales/
    ├── en/translation.json  # English — the reference file
    ├── de/translation.json
    ├── es/translation.json
    └── ... (24 locales in total)
```

## Adding or improving a translation

1. **For an existing locale** — open `ui/i18n/locales/<code>/translation.json` and fill in missing strings. The source of truth for meaning and terminology is always `ui/i18n/locales/en/translation.json`.

2. **For a new locale**
   - Check `ui/i18n/languages.ts` — the language list needs an entry with the right display name.
   - Create a new directory named after the locale code (e.g. `ui/i18n/locales/cs/`), copy `en/translation.json` into it as `translation.json`, and translate the values.
   - Do not rename keys, reorder the structure, add or remove keys.

## Golden rules

- **Never change keys.** The key structure must match the English file exactly.
- **Preserve placeholders.** Tokens such as `{{version}}`, `{{count}}`, or `{{model}}` must appear in the translated value exactly as written, with the same spelling between double braces.
- **Keep text natural**, not machine-literal. Say what the sentence means in your language, and use the standard punctuation of the language.
- **Brand style:** the app is called **voxix** — lowercase, not localized.
- **Model and product names stay untranslated** (Whisper, Parakeet, Moonshine, SenseVoice, `transcribe.cpp`, ggml, Hugging Face).

## Validating

After editing, run the parity check — it compares every locale's key set against English:

```bash
bun install
bun run check:translations
```

The check passes only when all locales have identical key parity. Fix any missing/extra keys it reports.

Then run the frontend build to make sure everything still compiles and screenshots/build cleanly:

```bash
bun run build
```

## Submitting

Open a pull request with your translation files (see [CONTRIBUTING.md](CONTRIBUTING.md) for the workflow) and mention the locale you worked on in the description.
