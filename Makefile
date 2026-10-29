# Пёрышко (Feather DE) — Makefile для сборки, упаковки и системной установки
PREFIX ?= /usr
BINDIR ?= $(PREFIX)/bin
DATADIR ?= $(PREFIX)/share
SYSCONFDIR ?= /etc
CARGO ?= cargo

BINARIES := feather-de feather-files feather-terminal feather-notes feather-preview feather-textedit feather-calc feather-installer

.PHONY: all build test install uninstall release clean

all: build

build:
	$(CARGO) build --release --workspace

test:
	dbus-run-session -- $(CARGO) test --release --workspace -- --include-ignored

install: build
	DESTDIR="$(DESTDIR)" PREFIX="$(PREFIX)" SYSCONFDIR="$(SYSCONFDIR)" ./packaging/install.sh --no-build

uninstall:
	DESTDIR="$(DESTDIR)" PREFIX="$(PREFIX)" SYSCONFDIR="$(SYSCONFDIR)" ./packaging/uninstall.sh

release:
	./packaging/build-release.sh

clean:
	$(CARGO) clean
