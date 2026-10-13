# Dev Container Setup Prompt

Copy the prompt below into an LLM session opened in the project you want to containerize. It asks for approval before creating a Dockerfile and build/run scripts, and covers tcode compatibility, persistent dependency caching, and host/container build isolation.

See [Container Mode](02-configuration.md#container-mode) for tcode's container configuration.

```text
You are working in the current project's directory.

Create a development container that provides the tools and dependencies
needed to develop, build, lint, and test this project, and that tcode can
use for its bash commands.

Required deliverables:
1. A Dockerfile.
2. A script that builds the development image.
3. A script that starts the development container.

The setup must:
- Use the live host checkout at the correct absolute path.
- Include the project's development dependencies.
- Persist useful dependency caches and incremental build state.
- Keep container dependency environments and build outputs separate from
  their host equivalents.
- Work with tcode's actual container execution behavior.

Do not create or modify files until you have inspected the project,
proposed a plan, and received the user's confirmation.

## 1. Inspect the project and host environment

Follow the project instructions already provided to you.

Identify:
- The project root and its absolute host filesystem path.
- Languages, frameworks, package managers, and required versions.
- Dependency manifests, lockfiles, toolchain files, workspace configuration,
  existing container files, CI workflows, and development documentation.
- Commands for dependency installation, building, testing, formatting,
  linting, and code generation.
- Required system packages, compilers, headers, native libraries, and tools.
- Existing dependency caches, installed dependency directories, build
  outputs, generated files, and test/lint caches.
- External services, environment variables, credentials, ports, external
  source directories, and symlink targets required for development.
- Host operating system, architecture, available container engines, and
  relevant rootless/rootful configuration.

Use declared versions and lockfiles where available. Do not substitute
arbitrary latest versions or modify manifests or lockfiles as part of
container setup.

Distinguish required development dependencies from optional tools and
production-only infrastructure.

If a requirement cannot be established from the project or environment,
ask the user rather than guessing.

## 2. Propose the design and obtain confirmation

Before writing files, summarize the proposed setup and explicitly ask the
user to confirm:
- Container engine: Docker or Podman.
- Build script filename and location.
- Run script filename and location.
- Dockerfile filename and location.

Suggested filenames, subject to confirmation:
- Dockerfile.dev
- build-dev-container.sh
- run-dev-container.sh

You may recommend an engine based on the environment, but do not choose
silently or automatically fall back to another engine.

Also propose:
- Image and container names.
- Base image and toolchain versions.
- System and project dependency installation approach.
- Source, HOME, cache, dependency, and build-output storage layout.
- Dependency initialization and synchronization behavior.
- Any ports, additional mounts, or external services.
- Existing-container reuse or recreation behavior.

Ask this additional question when generated files are involved:
"Do any generated source files or build artifacts need to be readable or
editable by tcode's host-side file tools or other host development tools?"

Explain consequential choices and unresolved requirements. Wait for
approval before creating or modifying files. Additional files or changes
outside the three deliverables require approval.

## 3. Understand tcode's container execution contract

tcode runs on the host and connects to an existing running container.
It does not build or start the development container.

Typical host-side invocations:

    tcode -c <container-name-or-id> --container-runtime docker

or:

    tcode -c <container-name-or-id> --container-runtime podman

The default runtime is Docker. Always print the explicit runtime in the
handoff command.

Each bash tool call starts a separate process equivalent to:

    <engine> exec \
      --user <uid>:<gid> \
      -e HOME=<host-home> \
      -w <absolute-host-workdir> \
      <container-name-or-id> \
      bash -c <command>

tcode adds an internal job marker for cancellation.

Important behavior:
- Docker: tcode uses the host user's UID and primary GID.
- Podman: tcode currently uses container UID/GID 0:0, assuming rootless
  Podman maps container root to the host user.
- Rootful Podman can therefore create root-owned host files. Detect and
  discuss this rather than assuming it is safe.
- Dockerfile USER and WORKDIR do not determine tcode's bash execution
  identity or working directory; exec arguments override them.
- Bash is non-interactive and non-login.
- Every call has independent shell state. Do not depend on previous exports,
  directory changes, virtualenv activation, .bashrc, or login profiles.
- Set required environment variables and PATH through image ENV, container
  creation options, or another reliable non-interactive configuration.
- HOME is explicitly set to the host HOME path, falling back to /root if
  unavailable. tcode does not automatically mount, create, or make it writable.
- Dedicated file tools operate on the host. Only bash execution is redirected
  into this container.
- tcode does not translate host paths into container paths.
- An explicitly supplied bash workdir must also exist on the host.
- Commands starting with `cd` are rejected by the bash tool; use its workdir
  parameter, keeping the selected directory available on both sides.
- The usual bash timeout is 120 seconds. Longer builds or tests may need
  an explicit timeout.

At startup, tcode checks that:
- The container is running.
- Bash is available.
- A marker created in the host working directory is visible at the
  identical absolute path inside the container.

This does not prove that tcode's eventual exec user can write to the
workspace or HOME.

Include:
- Bash.
- An executable `test` command, not only Bash's built-in.
- Cancellation utilities: procps-compatible `pgrep` and `ps`, plus `grep`,
  `sort`, `head`, and `tr`.
- The project's actual development dependencies.

Building and starting the container are host-side bootstrap operations.
Do not assume a tcode bash call already executing inside a container can
operate the host's Docker or Podman. Do not solve this by exposing a host
container socket or adding privileged mode without explicit approval.

## 4. Design dependency caching and build isolation

Treat these as distinct storage categories:
1. Download caches: package archives, registry metadata, Git dependencies.
2. Installed dependencies: node_modules, virtual environments, native
   packages, and other project-local dependency installations.
3. Build outputs: compiled objects, incremental compilation state, build
   trees, generated files, and test/lint caches.
4. Image-build caches: image layers and supported build-time cache mounts.

Image-build caching alone is insufficient. It does not automatically
provide persistent writable caches for later tcode commands.

Default policy:
- Share the live source checkout read-write.
- Persist container caches and build state across ordinary restarts,
  container recreation, and image rebuilds.
- Keep container-installed dependencies and compiled outputs separate from
  their host equivalents.
- Do not reuse host virtual environments or build trees by default.
- Never automatically delete, overwrite, or recursively change ownership
  of host dependency caches or build directories.

Prefer tool-supported cache and output configuration pointing to persistent
container-only storage outside the source bind mount.

When a tool requires a project-local directory, consider a nested named
volume at that directory so container writes do not touch the host's
equivalent directory.

Inspect actual project commands. Relocating a package download cache does
not necessarily relocate installed dependencies or build outputs.

Examples, only when applicable:
- Rust: separate CARGO_TARGET_DIR and container-specific Cargo dependency
  storage. Ensure storage mounts do not hide required image-installed tools.
- CMake: a separate out-of-source container build tree, not the host's build
  directory or CMakeCache.txt.
- JavaScript: persistent package-manager cache/store and isolated
  node_modules where dependencies may be platform-specific.
- Python: persistent package download cache, a container-specific virtual
  environment, and appropriate bytecode/test/lint cache locations.
- Other ecosystems: inspect their actual storage and output settings.

Use stable, project-specific volume names. Avoid collisions between
projects with the same basename.

Separate installed environments and compiled outputs where architecture,
OS, toolchain, interpreter, or ABI differences make reuse unsafe. Reuse
compatible download caches without discarding them unnecessarily after
every lockfile change.

Account for concurrent writers through tool-supported locking or separate
mutable storage.

Do not silently rewrite application build configuration. Propose any
necessary changes and obtain approval.

### Dependency initialization

Dependencies must remain usable after runtime mounts are attached:
- A source bind mount hides image contents at the mounted project path.
- A volume can hide image-installed contents at its destination.
- Existing volumes do not automatically receive updated contents from a
  rebuilt image.

Provide explicit, idempotent initialization or synchronization where needed.
Use lockfile-respecting commands and explain how initialization is rerun
when manifests or lockfiles change.

Verify volume permissions under tcode's actual exec UID/GID. Restrict any
ownership repair to container-owned storage, never the live checkout.

### Generated files and host visibility

Container-only volumes are not visible to tcode's host-side file tools.
Nested mounts can make the host and container see different contents at
the same apparent path.

If generated files must be host-visible, agree on specific non-conflicting
output paths or an explicit export step. Do not silently merge container
outputs into existing host build directories.

Document unavoidable source-tree writes. Do not promise isolation from
arbitrary commands: the source checkout is intentionally writable.

### Cache cleanup

Document volume identities and narrowly scoped reset commands.
Normal build/run operations must not discard caches.
Never automatically run broad volume pruning or remove unrelated storage.

## 5. Implement the approved Dockerfile

- Use a suitable maintained base image and required toolchain versions.
- Install the development tools, system packages, and native libraries.
- Ensure tools work under tcode's actual exec identity and non-login Bash.
- Configure stable PATH and relevant cache/output environment variables.
- Keep credentials and private configuration out of image layers.
- Do not rely on copied source as the development workspace: runtime uses
  the live host checkout.
- Ensure image-installed tools and dependencies remain available after
  mounts are attached.
- Use image-layer or build-time caching where supported by the confirmed
  engine, without assuming Docker-only features work with Podman.
- Avoid copying irrelevant host dependency trees and build outputs into the
  build context. Propose any necessary ignore-file changes for approval.
- Do not modify unrelated application code.

## 6. Implement the approved build script

- Use the confirmed engine, Dockerfile, and image name.
- Resolve the project/build context reliably, including when invoked from
  outside the project directory.
- Quote paths and support paths containing spaces.
- Fail clearly when required tools or inputs are unavailable.
- Pass necessary host UID/GID/HOME configuration consistently.
- Keep configuration consistent with the run script.
- Preserve useful image-build caches.
- Never silently switch engines or prune storage.

## 7. Implement the approved run script

- Start a detached, long-running container suitable for repeated exec calls.
- Use a predictable, project-specific container name.
- Bind-mount the actual absolute host project path read-write to the
  identical absolute container path.

  Example:
      host:      /home/alice/projects/example
      container: /home/alice/projects/example

  Mounting that checkout only at /workspace is not sufficient for tcode.

- Resolve symlinks carefully. Ensure the path tcode sees and required
  external symlink targets are available inside the container.
- Attach the approved persistent caches, dependency environments, and
  isolated build-output storage.
- Configure permissions for tcode's actual exec identity.
- Provide a writable HOME at the absolute path tcode forwards. Prefer an
  isolated container home/cache over mounting the entire host HOME.
- Account for nested mounts when the project is underneath HOME.
- Add only necessary mounts, environment variables, services, and ports.
- Avoid privileged mode, host container sockets, and broad host mounts
  unless explicitly required and approved.
- Do not use a Podman user-namespace arrangement that breaks tcode's forced
  0:0 identity. Verify the mapping.
- Handle existing containers explicitly. Do not silently delete them or
  reuse stale mounts/configuration.
- Complete required initialization before claiming the container is ready.
- Preserve persistent caches during ordinary startup and recreation.
- Make both scripts executable.
- Print the runtime, container name, mount layout, and exact host-side
  tcode invocation.

If the engine is remote or cannot access the host checkout, stop and explain
the limitation instead of claiming the bind mount works.

## 8. Verify the result

If host-side container execution is available and approved:
1. Build the image.
2. Start the container and confirm it remains running.
3. Verify Bash, executable `test`, and cancellation utilities.
4. Verify the checkout is visible at the identical absolute path.
5. Using tcode-equivalent exec arguments, verify:
   - Effective UID/GID.
   - Working directory.
   - HOME and PATH.
   - Workspace, HOME, and cache writability.
   - Toolchain versions and dependency availability.
6. Confirm host/container file visibility with a uniquely named disposable
   marker. Clean it up on success and failure.
7. Run appropriate dependency, build, lint, and test smoke checks.
8. Verify outputs and dependency environments use the approved isolated
   locations rather than existing host build/dependency directories.
9. Verify persistent state survives restart and, if approved, container
   recreation. Do not disrupt an active development container without
   permission.
10. Identify remaining source-tree writes and host-visibility limitations.

Do not modify or delete existing host build state merely to test isolation.

Do not claim verification succeeded if it could not be performed. Report
the limitation and provide exact remaining verification commands.

## 9. Final handoff

Summarize:
- Files created or changed.
- Confirmed engine and filenames.
- Image and container names.
- Installed toolchains and dependencies.
- Source, HOME, cache, dependency, and build-output storage.
- How dependencies are initialized and refreshed.
- How to build, start, stop, and intentionally recreate the container.
- What persists and how to reset only this project's container caches.
- The exact host-side tcode invocation.
- Verification results and remaining limitations.

Do not automatically change tcode's persistent project configuration.
Do not claim an already-running tcode runtime has switched containers.
Explain that a fresh runtime may be needed to apply the selected container.
```
