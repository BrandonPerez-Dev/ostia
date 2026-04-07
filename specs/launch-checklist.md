# Ostia Launch Checklist

## Phase 0: Make Public (now)
- [x] Apache 2.0 LICENSE file
- [x] .gitignore (target/, .claude/, .DS_Store)
- [ ] Push to GitHub (personal account)
- [ ] Set repo visibility to public

## Phase 1: Core Product (before any promotion)
- [ ] Dynamic profile-as-tool MCP redesign (each profile = own tool with CLI catalog in description)
- [ ] Fix symlinked binary mounting (python3 -> python3.11 class of bugs)
- [ ] Fix script-based tool support (npm, npx need runtime tree mounted)
- [ ] Mount /etc/resolv.conf + /etc/ssl/certs for DNS/TLS in sandbox
- [ ] Profile-specific MCP endpoints (/mcp/dev, /mcp/readonly)
- [ ] OSTIA_PORT env var in Dockerfile (configurable port without rebuilding)

## Phase 2: Repo Hygiene
- [ ] CONTRIBUTING.md
- [ ] SECURITY.md (vulnerability disclosure process)
- [ ] .github/ISSUE_TEMPLATE/ (bug report + feature request)
- [ ] GitHub Topics (sandbox, mcp, mcp-server, ai-agents, rust, landlock, seccomp, docker, linux-security, ai-safety, cli, devtools)
- [ ] Social preview image for repo (thumbnail on link shares)
- [ ] CI pipeline (GitHub Actions: cargo test, cargo clippy, cargo fmt --check)
- [ ] Tagged release v0.1.0 with GitHub release notes
- [ ] Pre-built binaries via cargo-dist or cross-compile workflow
- [ ] Docker image published to GHCR + Docker Hub

## Phase 3: Documentation
- [ ] README rewrite (tagline, demo GIF, quick start, how it works, feature list)
- [ ] Demo GIF with VHS (Charmbracelet) — show a tool getting blocked, then the policy that allows it
- [ ] One docker run command from zero to running in README
- [ ] Configuration reference (profile YAML schema, bundle system, auth modes)
- [ ] Examples directory (common profiles: readonly, dev, node, python, data-science)

## Phase 4: Community Seeding
- [ ] Submit PR to awesome-mcp-servers (wong2/awesome-mcp-servers, 20k+ stars)
- [ ] Submit PR to awesome-ai-agents
- [ ] Share with personal network (target 50-100 stars before formal launch)
- [ ] Dev.to article draft: "Building an OS-level sandbox for AI agents with Rust"

## Phase 5: Formal Launch
- [ ] Show HN post (Sunday 11:00-16:00 UTC / 7am-12pm ET)
- [ ] First comment ready (who you are, what problem, how it works technically, what's different from Docker alone)
- [ ] Tweet with demo GIF, link to HN thread
- [ ] Publish Dev.to article
- [ ] Reddit stagger: r/rust (day 1), r/AI_Agents + r/SideProject (day 1), r/LocalLLaMA + r/selfhosted (day 3), r/docker + r/ClaudeAI (day 5)
- [ ] Discord: Latent Space, Anthropic, Rust Community
- [ ] LinkedIn post

## Phase 6: Post-Launch
- [ ] Product Hunt submission (for the badge, ~2 weeks after HN)
- [ ] Monitor discussions with F5Bot (keywords: landlock, mcp security, ai agent sandbox)
- [ ] Evaluate org transfer (when: co-maintainer, business formation, or 1k+ stars)
