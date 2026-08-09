# Komiq README and Policy Safety Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Publish an accurate project README plus copyright and privacy policies that distinguish current behavior from unfinished compliance safeguards.

**Architecture:** Keep the GitHub landing page concise and place operational detail in two focused root-level policy documents. State only behavior verified in the repository or confirmed by the operator, and label every missing technical or operational safeguard as WIP.

**Tech Stack:** GitHub-flavored Markdown, repository architecture documentation, Rust/SQLite/Suwayomi service behavior, Cloudflare Workers edge caching.

---

### Task 1: Correct the GitHub project description

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Replace the inaccurate opening description**

Describe Komiq as an AGPL manga/comics reader for web, desktop, iOS, and Android.
Identify `komiq.cc` as a free WIP demo, not as a production or official content
service.

- [ ] **Step 2: Add the verified content flow**

Add a `How content works` section that states all of the following:

- eligible community members add third-party sources and additions become global;
- media is retrieved on demand in response to use of the reader;
- the web path may proxy and temporarily edge-cache image responses through Cloudflare;
- native builds may fetch directly or through the local Suwayomi engine;
- Komiq does not maintain a permanent first-party manga image/media library; and
- Komiq does store account, session, social, source/catalog, and operational data.

- [ ] **Step 3: Add the third-party content and donation boundaries**

State that third-party works remain their owners' property, Komiq is not affiliated
with the relevant publishers/source projects unless expressly stated, users must have
authorization and follow applicable law/source terms, and donations support software
development rather than content access.

- [ ] **Step 4: Link the policies**

Add repository-relative links to `COPYRIGHT.md` and `PRIVACY.md`. State that the
documents describe the Komiq-operated demo and that self-hosters are responsible for
their own deployment.

- [ ] **Step 5: Inspect the README diff**

Run `git diff -- README.md`.

Expected: the old absolute non-fetching/non-storage disclaimer is removed; setup,
workspace, status, and licensing instructions remain intact.

### Task 2: Add the copyright policy

**Files:**
- Create: `COPYRIGHT.md`

- [ ] **Step 1: Define scope and acceptable use**

Explain that Komiq is community-configured software/intermediary infrastructure and
does not grant rights to third-party works. Require users to add or access only sources
they are authorized to use and prohibit use that violates copyright, law, or source
terms.

- [ ] **Step 2: Define notice requirements**

Require a signature, identification of the protected works, precise Komiq and source
locations, claimant contact details, a good-faith unauthorized-use statement, and an
accuracy/authority statement. Mark the dedicated notice address and registered DMCA
agent contact as not yet established and required before public launch.

- [ ] **Step 3: Define response, counter-notice, and repeat-infringer policy**

Commit to good-faith review and appropriate disabling of URLs/sources, revocation of
trusted-user privileges, notice to affected users when appropriate, case records, and
proportionate repeat-infringer enforcement. Describe the statutory counter-notice
elements without guaranteeing restoration or claiming safe-harbor eligibility.

- [ ] **Step 4: Add the WIP compliance roadmap**

Include unchecked items for DMCA agent registration, dedicated intake, URL blocking,
source suspension, Cloudflare cache purge, case/audit records, automated repeat-
infringer enforcement, source provenance/reporting UI, and review by U.S. counsel.

- [ ] **Step 5: Inspect the copyright-policy diff**

Run `git diff --no-index /dev/null COPYRIGHT.md`.

Expected: unfinished controls use explicit `WIP` labels and unchecked boxes; no false
contact information or claim of DMCA qualification appears.

### Task 3: Add the privacy policy

**Files:**
- Create: `PRIVACY.md`

- [ ] **Step 1: Define scope and collected information**

Cover the Komiq-operated demo and distinguish self-hosted deployments. Document account
identifiers/password hashes, sessions, public social contributions, library/reading
state, local browser storage, source/catalog operational state, and normal network,
security, and infrastructure logs.

- [ ] **Step 2: Explain purposes, disclosure, and source requests**

Explain service delivery, authentication, social functionality, security, abuse
prevention, debugging, and legal compliance. Describe Cloudflare plus configured
hosting/infrastructure/backup providers as service-provider categories. Explain that
requests to third-party sources disclose ordinary request information to those sources.

- [ ] **Step 3: Explain retention, choices, and security**

Use conservative purpose-based retention language, identify public social visibility,
explain local-storage controls, avoid fixed deletion promises, and state that no
security method is perfect. Mark the privacy contact channel as required before public
launch rather than inventing it.

- [ ] **Step 4: Add the WIP privacy roadmap**

Include unchecked items for account export/deletion, a production retention schedule,
backup deletion handling, processor inventory, request tracking, and production data-
flow verification.

- [ ] **Step 5: Inspect the privacy-policy diff**

Run `git diff --no-index /dev/null PRIVACY.md`.

Expected: the policy does not claim that Komiq collects nothing, does not promise an
unimplemented deletion mechanism, and marks missing operational channels WIP.

### Task 4: Verify consistency and links

**Files:**
- Verify: `README.md`
- Verify: `COPYRIGHT.md`
- Verify: `PRIVACY.md`

- [ ] **Step 1: Search for unsafe absolute claims**

Run:

```bash
rg -n -i 'does not fetch|doesn.t fetch|does not store anything|stores? nothing|storageless|no images are saved' README.md COPYRIGHT.md PRIVACY.md
```

Expected: no matches.

- [ ] **Step 2: Verify required headings and WIP disclosures**

Run:

```bash
rg -n '^## |WIP|\[ \]' README.md COPYRIGHT.md PRIVACY.md
```

Expected: README contains content/policy sections; both policy files contain clearly
labeled WIP roadmaps with unchecked implementation items.

- [ ] **Step 3: Verify internal links**

Run:

```bash
for path in SPEC.md LICENSE NOTICE COPYRIGHT.md PRIVACY.md; do test -f "$path" || exit 1; done
```

Expected: exit status 0.

- [ ] **Step 4: Review the complete change set**

Run `git diff --check`, `git status --short`, and
`git diff -- README.md COPYRIGHT.md PRIVACY.md`.

Expected: no whitespace errors; the design/plan commits remain separate; only the
requested README and policy work is uncommitted for the implementation commit.

- [ ] **Step 5: Commit the documentation**

Run:

```bash
git add README.md COPYRIGHT.md PRIVACY.md
git commit -m "docs: clarify content and privacy policies"
```

Expected: one documentation commit containing the README rewrite and both policies.
