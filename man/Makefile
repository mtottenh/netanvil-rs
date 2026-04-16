PREFIX ?= /usr/local
MANDIR ?= $(PREFIX)/share/man

SCDOC_SOURCES := $(wildcard *.scd)
MAN_PAGES := $(patsubst %.scd,%,$(SCDOC_SOURCES))
GENERATED := $(addprefix generated/,$(MAN_PAGES))

.PHONY: man install-man clean

man: $(GENERATED)

generated/%: %.scd
	@mkdir -p generated
	scdoc < $< > $@
	@echo "  $(notdir $@)"

install-man: man
	@for page in $(GENERATED); do \
		section=$$(echo "$$page" | grep -oP '\.\K[0-9]+$$'); \
		install -Dm644 "$$page" "$(DESTDIR)$(MANDIR)/man$$section/$$(basename $$page)"; \
		echo "  installed $$(basename $$page) -> man$$section/"; \
	done

clean:
	rm -rf generated
