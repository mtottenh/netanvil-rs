# Man Pages

Man pages for netanvil utilities, authored in [scdoc](https://git.sr.ht/~sircmpwn/scdoc) format.

## Prerequisites

Install scdoc:

```sh
# Debian/Ubuntu
apt install scdoc

# macOS
brew install scdoc

# Arch
pacman -S scdoc
```

## Building

```sh
# From the repo root:
./man/build.sh

# Or via make:
make -C man man
```

Generated roff files are written to `man/generated/` (git-ignored).

## Previewing

```sh
man -l man/generated/netanvil-test-server.1
```

## Installing

```sh
make -C man install-man                           # -> /usr/local/share/man/
make -C man install-man PREFIX=/usr               # -> /usr/share/man/
make -C man install-man DESTDIR=/tmp/pkg PREFIX=/usr  # for packaging
```

## Adding a new page

1. Create `man/<name>.<section>.scd` (e.g. `netanvil-cli.1.scd`).
2. Run `./man/build.sh` -- it picks up new `.scd` files automatically.
3. No registration step required.
