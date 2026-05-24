# csilgen-requests

This directory is the inbox for **requests from other repos** that consume csilgen. Sister repos (longhouse, foundry, etc.) drop markdown files here describing what they need from csilgen — new generator features, output-shape changes, target-name additions, options, fixes — so the work is version-controlled alongside the calling code and visible to whoever picks it up here.

## Conventions

- One file per request, named after the feature/topic (e.g. `typescript-streaming-client.md`, `rust-bytes-handling.md`).
- Start with **Status** (`open`, `in-progress`, `done`, `deferred`) and a one-line summary.
- Describe the problem from the *consumer's* perspective, then what's needed from csilgen. Avoid prescribing csilgen internals — that's for the csilgen author to design.
- When the work lands, either delete the file or flip it to `Status: done` with a short note pointing at the implementation.

The directory may be empty when there's nothing pending. That's the success state.
