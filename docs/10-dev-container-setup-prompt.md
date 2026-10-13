# Dev Container Setup Prompt

Copy the text below into a session opened in the project you want to containerize.

```text
Create a development container for the project in the current directory.

## Deliverables

- Dockerfile.dev installs the required development tools and native dependencies at approved versions.
- build-dev-container.sh builds the approved image with the confirmed engine and resolves its
  context from its own location, even when invoked outside the project.
- run-dev-container.sh initializes the container, opens an interactive shell, handles storage
  and cleanup, and prints host tcode usage and an optional default-configuration hint.

These are the only required deliverables, at the confirmed locations. Do not edit the README by default.

## Before changing files

Follow supplied project instructions. Inspect the project's name and physical absolute path,
required external sources and symlink targets, languages, manifests, lockfiles, declared versions,
commands for building, testing, formatting, linting and generating code, native dependencies,
services, environment variables, ports, secrets, and existing caches, dependencies and outputs.
Distinguish required development tools from optional or production tools. Check the host operating
system, architecture, available engines, and whether the engine is local and rootless.
Limit inspection to the project, requested host configuration files, and exact named resources.

Summarize the plan and confirm Docker or Podman, suggested filenames and locations, project-based
image, container and volume names, versions, optional tools, mounts, storage, and ports.
Never silently switch engines. Ask whether generated outputs need to be visible to host file tools;
agree on exports or shared paths without reusing host builds. Wait for confirmation before
implementing. Extra files or application configuration changes require approval.

## Requirements

- Keep scripts short and executable, without generalized management. Write discovered paths and
  names directly into the simple run command.
- Build and start the container on the host. Mount the live checkout read-write at the same
  absolute path, including required external sources and symlink targets. Explicit working
  directories must exist on both sides. Stop if a remote engine cannot access the checkout.
- Use declared versions and lockfiles without changing manifests or choosing arbitrary latest
  versions. Do not bake secrets into the image or copy source as the workspace. Use only approved
  services, environment variables, and ports.
- Include Bash, an external test executable, procps-compatible pgrep and ps, grep, sort, head, and tr.
  Set required PATH, cache and output environment variables in the image or container configuration,
  without relying on activation or shell profiles.
- Default to two project-named persistent volumes. Use separate folders for downloads and installed
  dependencies in one, and builds, generated files, test and lint state in the other. Both must
  survive stopping, recreation, and image rebuilds, independently of host dependencies and builds.
  Persist Rust incremental state; image layers alone cannot preserve runtime caches.
  Limit any reset instructions to these project volumes.
- Use the tools' supported cache and output paths, or mount volume subdirectories where needed.
  Mounts must not hide installed tools or copy host dependencies or builds. Initialize dependencies
  after mounts are attached, respecting lockfiles without changing them. This must be safe to repeat
  and finish successfully before opening the shell. Explain how to refresh dependencies or rerun
  initialization after manifests, lockfiles, or toolchains change.
- Limit ownership fixes to container-owned storage. Never recursively change ownership or delete the
  host checkout or caches, or prune unrelated storage. Explain that commands can write to the checkout
  and that nested mounts can hide generated outputs from host tools.
- Run in the foreground and open an interactive Bash shell unless the user asks otherwise.
  Remove the container automatically after exit or interruption, releasing its name and preserving
  both volumes. Before startup, remove only stopped leftovers of the approved name; a missing name
  is harmless. Never force-stop a running container without approval.
- Provide a writable, isolated HOME at the forwarded absolute path, without a persistent volume by
  default. It survives stopping and starting the same container, but not recreation. Never mount the
  entire host HOME. Propose read-only mounts of relevant existing Git configuration, including
  .gitconfig and .global_ignore, at identical absolute paths. Check whether requested files exist;
  read private contents only when necessary and approved. Ask about other includes or helpers.
  Credentials, keys, engine sockets, broad mounts, and privilege require approval.

### Run script output

Have the run script print usage and tell the user to keep the foreground shell open and launch
host tcode from another host terminal at the project root with explicit arguments:

    tcode -c <container-name> --container-runtime <confirmed-runtime>

Include the optional default-configuration hint only in the run script's output, not in the README
or final handoff.
The hint must tell the user to run tcode project-config-path on the host from the physical project
root and explain that it prints a directory, not a file path. It must tell the user to edit that
directory's config.toml, creating the directory and file if needed while preserving existing keys
and sections. It must specify top-level container = "<container-name>" and
container_runtime = "<confirmed-runtime>" using the confirmed name and runtime before any section
headers, then tell the user to start a fresh plain host tcode session from the same root while the
container is running. Do not modify configuration automatically.

## tcode-specific behavior

tcode runs on the host and requires a container that is already running; it does not build or start
one. Each Bash call runs independently in a non-interactive, non-login shell. It runs the equivalent of:

    <engine> exec --user <uid>:<gid> -e HOME=<host-home> -w <absolute-host-workdir> <container> bash -c <command>

tcode adds a cancellation job marker. For Docker, it uses the host UID and primary GID. For Podman,
it forces 0:0 and expects a rootless engine to map this to the host user. Guard against rootful Podman
writing root-owned files; --userns keep-id can break this mapping. The exec arguments override USER
and WORKDIR. tcode forwards host HOME (or /root) but does not create or mount it; the container must
provide it. File tools remain on the host, without path translation. At startup, tcode checks that
the container is running, Bash is available, and a host marker is visible at the same path. It does
not check whether its execution user can write. Bash calls default to a 120-second timeout;
allow longer for heavy checks.

## Verification

Ask separately before building, starting, or testing containers; file approval does not authorize
these operations. Skip runtime checks if the user already tested or declines. Static checks are
fine. With permission, use tcode's exec arguments to verify identity, working directory, HOME,
PATH, writable checkout, HOME and storage, same-path marker visibility, tool versions, dependencies,
isolated outputs, persistence, and agreed build, lint and test commands. Clean up disposable markers.
Protect active containers and host builds, and skip unapproved paid or optional tests.

## Finish

Give a brief handoff in clear, complete plain sentences explaining how to build and run with the
scripts from the host, use host tcode, and exit the shell or rerun the run script. Mention any
remaining relevant verification limitations, including checks that were not run. Ask whether the
user wants these usage instructions added to the README. Wait for explicit approval before editing
the README; a README update is not part of the default plan.
```
