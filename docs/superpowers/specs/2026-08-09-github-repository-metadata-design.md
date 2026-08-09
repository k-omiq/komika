# GitHub Repository Metadata Design

**Date:** 2026-08-09  
**Status:** Approved for implementation

## Goal

Update the public metadata for the GitHub repository `k-omiq/komika` so it speaks
primarily to developers and self-hosters while still explaining the product clearly.

## Metadata

### Description

Use this exact repository description:

> Open-source, self-hostable manga and comics reader and social platform powered by Suwayomi, built with Rust, SvelteKit, Tauri, and GraphQL.

The description combines the project's self-hosting/platform positioning with its
principal implementation stack. It avoids unsupported claims such as "storageless"
and avoids positioning the project as an aggregator or scraper.

### Website

Set the repository website to:

> https://komiq.cc

### Topics

Replace the repository topic set with the following topics:

- `manga-reader`
- `comic-reader`
- `self-hosted`
- `suwayomi`
- `social-platform`
- `sveltekit`
- `tauri`
- `rust`
- `graphql`
- `cloudflare-workers`
- `desktop-app`
- `ios`
- `android`
- `agpl`

The topics balance discoverability by product type, deployment model, architecture,
platform, and license. They intentionally omit inaccurate or unnecessarily risky
positioning such as `storageless`, `aggregator`, `scraper`, `piracy`, or
`unofficial-sources`.

## Scope

Only these GitHub repository fields will change:

- description;
- website/homepage URL; and
- topics.

The repository name, visibility, default branch, features, permissions, releases,
and source files are outside this metadata change. The design document itself is the
only local repository change required by the design workflow.

## Application and Error Handling

Read the current repository metadata before applying changes. Update all approved
fields in one operation when the available GitHub tooling supports it; otherwise,
apply the description/homepage and topics sequentially. If authentication or repository
administration permission is missing, stop without attempting alternate accounts or
changing repository visibility or permissions.

## Verification

After the update, read `k-omiq/komika` from GitHub again and verify:

1. The description exactly matches the approved sentence.
2. The homepage URL is exactly `https://komiq.cc` after normal URL normalization.
3. The topic set contains exactly the fourteen approved topics, with no unexpected
   additions or omissions.
4. No unrelated repository settings changed.

Verification must use GitHub's returned repository metadata rather than assuming that
a successful update request changed every field.
