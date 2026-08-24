---
title: Add users and collaborate
description: Create Grantspace accounts, configure email, invite teammates, assign roles, and share changes safely.
---

<div class="guide-layout">
<aside class="toc">
<strong>On this page</strong>
<a href="#create-user-accounts">Create user accounts</a>
<a href="#configure-email">Configure email</a>
<a href="#invite-to-a-grant">Invite to a grant</a>
<a href="#project-roles">Project roles</a>
<a href="#work-together">Work together</a>
<a href="#account-recovery">Account recovery</a>
</aside>
<article class="guide-content" markdown="1">

<div class="eyebrow">Team workflow</div>
# Add users and collaborate

Account creation and grant access are separate. The system administrator first creates a login; a project owner, PI, or research administrator then invites that matching email address into a grant.

## Create user accounts

The first administrator opens **Administration → Account administration** and enters:

- a unique username;
- the user's email address and display name;
- a temporary password.

The user signs in with the username and temporary password, then must choose a new password before using the application.

## Configure email

Grantspace uses the configured SMTP server for new-account messages, password-reset links, and project invitations. Add these values to `.env`:

```bash
APP_PUBLIC_URL=https://grantspace.example.org
SMTP_HOST=smtp.example.org
SMTP_PORT=587
SMTP_SECURITY=starttls
SMTP_TIMEOUT_SECONDS=30
SMTP_USERNAME=grantspace-smtp-user
SMTP_PASSWORD=replace-with-secret
SMTP_FROM=grantspace@example.org
```

Use `SMTP_SECURITY=tls` for implicit TLS or `none` only for a trusted development relay. `SMTP_USERNAME` and `SMTP_PASSWORD` must either both be set or both be blank.

Restart the application after changing `.env`:

```bash
./stop.sh
./start.sh
```

<div class="callout warning"><strong>“Accepted” is not “delivered.”</strong> A success message means the SMTP server accepted the email. Final inbox delivery still depends on the relay, DNS, spam controls, and recipient server.</div>

## Invite to a grant

Open the grant, then **Team → Members & invitations**:

1. enter the exact email used by the teammate's account;
2. select a project role and expiration period;
3. choose **Create and email invitation**;
4. have the teammate sign in and open the single-use link.

<figure class="screenshot">
  <img src="{{ '/assets/images/team-management.png' | relative_url }}" alt="Grantspace team management page listing members and an invitation form">
  <figcaption>Project invitations are time-limited and must match the authenticated account email.</figcaption>
</figure>

If SMTP is not configured, Grantspace can create the invitation but cannot deliver it. Configure SMTP before relying on email invitations.

## Project roles

| Role | Typical responsibility |
|---|---|
| Project owner | Manages membership, workflow configuration, and high-level decisions. |
| Principal investigator | Leads scientific content, approvals, and project direction. |
| Contributor / scientific writer | Edits sections, adds guidance, and completes assigned work. |
| Reviewer | Reviews shared content and records feedback. |
| Approver | Records approval decisions where the workflow calls for them. |
| Research administrator | Manages the grant process, invitations, tasks, and submission work. |
| Viewer | Reads project material without edit privileges. |

## Work together

Grantspace stores collaboration in the shared project, including section guidance, comments, channels, tasks, notifications, activity, and named approvals. A user sees their own save immediately; teammates use **Refresh shared changes** to pull the latest server state.

Saves create immutable versions. If someone edits from an outdated base version, the application reports a stale edit instead of silently overwriting newer work.

<div class="callout"><strong>No live cursor claim.</strong> The current collaboration model is shared persistence plus explicit refresh, version history, and conflict detection—not character-by-character co-editing.</div>

## Account recovery

Users can request a password-reset link from the login page. The administrator can also issue an account reset from **Administration**. Both flows require working SMTP delivery.

</article>
</div>

