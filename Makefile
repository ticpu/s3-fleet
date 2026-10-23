# Release artifacts: two static musl binaries from one container pass, their
# man pages, and a .deb per architecture.
#
#   make            dist/s3-fleet.{amd64,arm64} and dist/man/*.1
#   make deb        s3-fleet_<version>_{amd64,arm64}.deb
#
# The version is Cargo.toml's on a clean tagged checkout, and gets a
# +<commits>-<sha> suffix otherwise, so an untagged build never poses as a
# release.

NAME := s3-fleet
CARGO_VERSION := $(shell grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)
VERSION := v$(CARGO_VERSION)
GIT_DIRTY := $(shell git diff-index --quiet HEAD -- . || echo dirty)
GIT_TAG := $(shell git tag --points-at HEAD | grep -E '^v')
GIT_VERSION := $(shell git log --oneline . | wc -l)-$(shell git rev-parse --short HEAD)
BASE_VERSION := $(if $(GIT_DIRTY),$(VERSION)+$(GIT_VERSION),$(if $(GIT_TAG),$(VERSION),$(VERSION)+$(GIT_VERSION)))
DEB_VERSION := $(patsubst v%,%,$(BASE_VERSION))
DEB_AMD64 := $(NAME)_$(DEB_VERSION)_amd64.deb
DEB_ARM64 := $(NAME)_$(DEB_VERSION)_arm64.deb
BINARIES := dist/$(NAME).amd64 dist/$(NAME).arm64
MAN_MAIN := dist/man/$(NAME).1
CONTAINER := $(shell command -v podman || command -v docker)
MAINTAINER := $(shell sed -n 's/^Maintainer: //p' DEBIAN/control)
# The commit's date, not the build's, so a rebuild is byte-identical.
CHANGELOG_DATE := $(shell git log -1 --format=%aD)
# Debian names it changelog.gz for a native version (no hyphen), else
# changelog.Debian.gz.
CHANGELOG := $(if $(findstring -,$(DEB_VERSION)),changelog.Debian.gz,changelog.gz)
SOURCES := Containerfile Cargo.toml $(wildcard Cargo.lock) $(shell find src -name '*.rs')

.PHONY: all binaries deb clean

all: binaries

binaries: $(BINARIES) $(MAN_MAIN)

deb: $(DEB_AMD64) $(DEB_ARM64)

# One container build makes both binaries and the man pages, so they share
# a single rule.
$(BINARIES) $(MAN_MAIN) &: $(SOURCES)
	@test -n "$(CONTAINER)" || { echo "neither podman nor docker found" >&2; exit 1; }
	rm -rf dist
	$(CONTAINER) build -f Containerfile --output type=local,dest=dist .

define build-deb
	rm -rf "package.$(1)"
	install -D -m 755 -T "$<" "package.$(1)/usr/bin/$(NAME)"
	install -d "package.$(1)/usr/share/man/man1"
	for page in dist/man/*.1; do \
		gzip -9n < "$$page" > "package.$(1)/usr/share/man/man1/$${page##*/}.gz" || exit 1; \
	done
	install -D -m 644 -T packaging/$(NAME).bash \
		"package.$(1)/usr/share/bash-completion/completions/$(NAME)"
	install -D -m 644 -T packaging/copyright "package.$(1)/usr/share/doc/$(NAME)/copyright"
	install -D -m 644 -T packaging/lintian-overrides \
		"package.$(1)/usr/share/lintian/overrides/$(NAME)"
	printf '%s\n' \
		"$(NAME) ($(DEB_VERSION)) unstable; urgency=medium" "" \
		"  * Release notes: https://github.com/ticpu/$(NAME)/releases/tag/$(VERSION)" "" \
		" -- $(MAINTAINER)  $(CHANGELOG_DATE)" \
		| gzip -9n > "package.$(1)/usr/share/doc/$(NAME)/$(CHANGELOG)"
	install -D -m 644 -T DEBIAN/control "package.$(1)/DEBIAN/control"
	sed -i -e "s/^Version:.*/Version: $(DEB_VERSION)/" \
		-e "s/^Architecture:.*/Architecture: $(1)/" \
		"package.$(1)/DEBIAN/control"
	@if grep -rq "$$PWD" "package.$(1)"; then echo "ERROR: package contains build path ($$PWD)" >&2; exit 1; fi
	dpkg-deb --build --root-owner-group "package.$(1)" "$@"
	rm -rf "package.$(1)"
endef

$(DEB_AMD64): dist/$(NAME).amd64 $(MAN_MAIN) DEBIAN/control $(wildcard packaging/*)
	$(call build-deb,amd64)

$(DEB_ARM64): dist/$(NAME).arm64 $(MAN_MAIN) DEBIAN/control $(wildcard packaging/*)
	$(call build-deb,arm64)

clean:
	rm -rf dist package.* *.deb
