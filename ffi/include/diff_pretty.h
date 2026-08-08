#ifndef DIFF_PRETTY_H
#define DIFF_PRETTY_H

#include <stddef.h>
#include <stdint.h>

#define DIFF_PRETTY_ABI_VERSION 1u

#define DIFF_PRETTY_PAGING_AUTO 0u
#define DIFF_PRETTY_PAGING_ALWAYS 1u
#define DIFF_PRETTY_PAGING_NEVER 2u

#define DIFF_PRETTY_STATUS_OK 0
#define DIFF_PRETTY_STATUS_QUIT 1
#define DIFF_PRETTY_STATUS_ERROR -1
#define DIFF_PRETTY_STATUS_INVALID -2

/* These values mirror Git 2.55's private enum diff_symbol. */
enum diff_pretty_event_kind {
	DIFF_PRETTY_EVENT_BINARY_DIFF_HEADER = 0,
	DIFF_PRETTY_EVENT_BINARY_DIFF_HEADER_DELTA = 1,
	DIFF_PRETTY_EVENT_BINARY_DIFF_HEADER_LITERAL = 2,
	DIFF_PRETTY_EVENT_BINARY_DIFF_BODY = 3,
	DIFF_PRETTY_EVENT_BINARY_DIFF_FOOTER = 4,
	DIFF_PRETTY_EVENT_STATS_SUMMARY_NO_FILES = 5,
	DIFF_PRETTY_EVENT_STATS_SUMMARY_ABBREV = 6,
	DIFF_PRETTY_EVENT_STATS_SUMMARY_INSERTS_DELETES = 7,
	DIFF_PRETTY_EVENT_STATS_LINE = 8,
	DIFF_PRETTY_EVENT_WORD_DIFF = 9,
	DIFF_PRETTY_EVENT_STAT_SEP = 10,
	DIFF_PRETTY_EVENT_SUMMARY = 11,
	DIFF_PRETTY_EVENT_SUBMODULE_ADD = 12,
	DIFF_PRETTY_EVENT_SUBMODULE_DEL = 13,
	DIFF_PRETTY_EVENT_SUBMODULE_UNTRACKED = 14,
	DIFF_PRETTY_EVENT_SUBMODULE_MODIFIED = 15,
	DIFF_PRETTY_EVENT_SUBMODULE_HEADER = 16,
	DIFF_PRETTY_EVENT_SUBMODULE_ERROR = 17,
	DIFF_PRETTY_EVENT_SUBMODULE_PIPETHROUGH = 18,
	DIFF_PRETTY_EVENT_REWRITE_DIFF = 19,
	DIFF_PRETTY_EVENT_BINARY_FILES = 20,
	DIFF_PRETTY_EVENT_HEADER = 21,
	DIFF_PRETTY_EVENT_FILEPAIR_PLUS = 22,
	DIFF_PRETTY_EVENT_FILEPAIR_MINUS = 23,
	DIFF_PRETTY_EVENT_WORDS_PORCELAIN = 24,
	DIFF_PRETTY_EVENT_WORDS = 25,
	DIFF_PRETTY_EVENT_CONTEXT = 26,
	DIFF_PRETTY_EVENT_CONTEXT_INCOMPLETE = 27,
	DIFF_PRETTY_EVENT_PLUS = 28,
	DIFF_PRETTY_EVENT_MINUS = 29,
	DIFF_PRETTY_EVENT_CONTEXT_FRAGINFO = 30,
	DIFF_PRETTY_EVENT_CONTEXT_MARKER = 31,
	DIFF_PRETTY_EVENT_SEPARATOR = 32,
};

struct diff_pretty_config {
	uint32_t version;
	uint32_t size;
	uint32_t paging;
	int32_t output_fd;
	int32_t tty_fd;
};

struct diff_pretty_session;

struct diff_pretty_session *diff_pretty_begin(
	const struct diff_pretty_config *config);

int diff_pretty_push_patch(struct diff_pretty_session *session,
			   const unsigned char *data, size_t len);
int diff_pretty_push_event(struct diff_pretty_session *session,
			   uint32_t kind, uint32_t flags,
			   const unsigned char *data, size_t len);
int diff_pretty_finish(struct diff_pretty_session *session);
int diff_pretty_page(struct diff_pretty_session *session);
const char *diff_pretty_last_error(const struct diff_pretty_session *session);
void diff_pretty_abort(struct diff_pretty_session *session);

#endif
