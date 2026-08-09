# Komiq README and Policy Safety Design

**Date:** 2026-08-09  
**Status:** Approved for implementation

## Purpose

Replace inaccurate statements in the repository README with a factual description
of Komiq's content flow and add concise copyright and privacy policies for the
United States-operated `komiq.cc` work-in-progress demo.

The documentation must improve transparency without claiming that a disclaimer
creates legal immunity, that Komiq currently qualifies for a statutory safe harbor,
or that unfinished compliance controls already exist. This work is not a substitute
for advice from a qualified attorney.

## Confirmed Operating Model

- `komiq.cc` is a work-in-progress demonstration of the desktop and mobile apps.
- Eligible users can add third-party sources. An accepted user addition becomes
  globally available immediately and automatically; it is not individually selected
  by the operator.
- Komiq retrieves media from community-added third-party sources when requested.
- The web image path may temporarily store responses in Cloudflare's edge cache.
  Komiq does not maintain a permanent first-party media or manga-image library.
- The native apps fetch source media directly or through their local Suwayomi engine,
  depending on the build and configuration.
- Komiq does store first-party service data, including accounts, authentication
  sessions, reviews, comments, administrative state, and source/catalog operational
  metadata. Some social data can also be stored locally in a user's browser.
- The service is free. Voluntary donations support software development and do not
  purchase content, access, or additional source privileges.
- Komiq is not affiliated with or endorsed by publishers, authors, artists,
  scanlators, source operators, Suwayomi, Mihon, or extension authors unless expressly
  stated.

## Documentation Structure

### README.md

The README will:

1. Introduce Komiq consistently and identify the hosted site as a WIP demo.
2. Add a compact "How content works" section that explains:
   - community-added global sources;
   - user-requested, on-demand media retrieval;
   - temporary Cloudflare edge caching for the web path;
   - the absence of a permanent first-party manga-media library; and
   - the existence of first-party account, social, and operational data.
3. Add a "Third-party content" section covering ownership, lack of affiliation,
   source responsibility, and the absence of any grant of content rights.
4. Explain that donations support development only.
5. Link to the copyright and privacy policies.
6. Retain the existing setup, development, workspace, status, license, and attribution
   information unless a statement conflicts with the verified operating model.

The README will not say that Komiq "does not fetch," "does not store anything," or
that all activity is private to one user's device. It will not describe Komiq as
legally protected by the DMCA or any other safe harbor.

### COPYRIGHT.md

The copyright policy will:

1. State that Komiq is a software project and community-configured intermediary,
   not a publisher or owner of third-party works.
2. Require users to add and use only sources and material they are authorized to
   access and that comply with applicable law and source terms.
3. Explain how a rightsholder or authorized agent can submit a notice identifying
   the protected work, the specific Komiq/source URLs, contact details, good-faith
   belief, accuracy/authority statement, and signature.
4. Establish an operator response process: acknowledge and review notices, disable
   affected Komiq access where appropriate, suspend or remove sources, and preserve
   an internal case record.
5. Describe counter-notice requirements without promising automatic restoration.
6. Establish a repeat-infringer policy covering accounts and trusted-source
   privileges, with room for case-specific judgment and mistaken notices.
7. Prohibit abuse of the reporting process and reserve action against bad-faith or
   materially false submissions.
8. Separate present policy commitments from a checklist titled "Technical and
   operational compliance roadmap (WIP)."

The WIP checklist will identify controls not yet evidenced in the repository:

- designate and register a DMCA agent and publish complete agent contact details;
- create a dedicated copyright-notice intake address or form;
- implement source-level suspension and URL-level blocking;
- purge affected objects from Cloudflare edge cache;
- create notice, decision, and response audit records;
- automate repeat-infringer and trusted-source privilege enforcement;
- expose user-facing source provenance and reporting controls; and
- obtain a U.S. copyright attorney's review before relying on a safe harbor.

Until a real copyright contact exists, the policy will use a conspicuous placeholder
that says it must be replaced before public launch. It will not invent an address,
phone number, person, or agent registration.

### PRIVACY.md

The privacy policy will:

1. Identify the policy's scope: the public demo, Komiq-operated services, and the
   distinction between those services and self-hosted installations.
2. Describe the categories of information currently evidenced by the repository:
   account identifiers and password hashes, authentication sessions, reviews and
   comments, reading/library state passed through Suwayomi, local browser storage,
   source/catalog operational state, and ordinary infrastructure/security logs.
3. Explain the operational purposes for those categories.
4. Identify relevant service-provider categories, including Cloudflare and hosting,
   database-backup, and infrastructure providers when configured, while avoiding
   unsupported claims about exact processor configurations.
5. State that public social submissions may be visible to other users.
6. Use conservative retention language: information is retained while an account or
   service purpose remains and as reasonably needed for security, legal compliance,
   dispute handling, and backups. It will not promise deletion periods the software
   cannot enforce.
7. Provide a contact placeholder for privacy requests and explicitly mark it as a
   pre-launch requirement.
8. Explain that no security method is perfect and avoid absolute security promises.
9. Include a "Privacy engineering roadmap (WIP)" for account export/deletion,
   published retention schedules, backup deletion handling, processor inventory,
   request tracking, and production data-flow verification.

## Policy Boundaries

- Policy text will describe how the operator intends to respond but will not certify
  eligibility for 17 U.S.C. section 512.
- Calling the service a demo, free, community-configured, or noncommercial does not
  eliminate copyright, privacy, contract, or platform-policy obligations.
- Temporary automatic caching is described as storage; the documentation will not
  equate the absence of permanent object storage with "no storage."
- User selection is described narrowly. Eligible users add sources, but sources then
  become global, and Komiq may perform shared or automated catalog operations.
- Copyright ownership and source authorization are separate from technical write or
  modification permissions.
- The policies cover Komiq-operated services. Third-party self-hosters remain
  responsible for their own operation, configuration, notices, and compliance.

## Verification

Implementation verification will include:

1. Inspecting the final README and policy text for contradictions with the repository.
2. Searching documentation for absolute claims such as "does not fetch," "does not
   store anything," and unqualified "storageless" language.
3. Checking that all internal Markdown links resolve.
4. Checking that every unfinished safeguard is marked WIP and that no placeholder is
   presented as a working contact channel.
5. Reviewing the final diff to ensure unrelated working-tree changes are preserved.

No application code or infrastructure behavior will be changed in this documentation
task. The WIP roadmap will make those follow-up changes visible without implying they
are complete.
