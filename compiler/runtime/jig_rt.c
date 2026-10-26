/* The Jig runtime. See jig_rt.h for conventions. */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif
#include "jig_rt.h"

#include <dirent.h>
#include <netdb.h>
#include <sys/socket.h>
#include <pthread.h>
#include <errno.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

/* ---------- process ---------- */

static int g_argc;
static char **g_argv;
static char g_out[1 << 16];
static size_t g_out_len;
static uint64_t g_rng;

static void flush_out(void) {
    if (g_out_len) {
        fwrite(g_out, 1, g_out_len, stdout);
        g_out_len = 0;
    }
    fflush(stdout);
}

void jig_init(int argc, char **argv) {
    g_argc = argc;
    g_argv = argv;
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    g_rng = ((uint64_t)ts.tv_nsec * 0x9e3779b97f4a7c15ull) ^ ((uint64_t)getpid() << 32) ^ (uint64_t)ts.tv_sec;
    if (!g_rng) g_rng = 1;
    atexit(flush_out);
}

static void *run_thunk(void *f) {
    ((void (*)(void))f)();
    return NULL;
}

void jig_run_main(void (*f)(void)) {
    /* Deeply recursive programs get the same headroom as under the test runner. */
    pthread_attr_t attr;
    pthread_t t;
    pthread_attr_init(&attr);
    if (pthread_attr_setstacksize(&attr, (size_t)1 << 30) != 0 || pthread_create(&t, &attr, run_thunk, (void *)f) != 0) {
        f();
        return;
    }
    pthread_join(t, NULL);
}

void jig_exit(int code) {
    flush_out();
    exit(code);
}

void jig_panic(const char *msg, const char *loc) {
    /* With tasks, several threads may fail at once: the first one reports. */
    static int panicking;
    if (__atomic_exchange_n(&panicking, 1, __ATOMIC_SEQ_CST))
        for (;;) pause();
    flush_out();
    fprintf(stderr, "panic at %s: %s\n", loc, msg);
    fflush(stderr);
    _exit(101);
}

void jig_panic_buf(jig_buf *b, const char *loc) {
    jig_buf_push(b, "", 1);
    jig_panic(b->data, loc);
}

void jig_unreachable(const char *loc) { jig_panic("entered unreachable code", loc); }

/* ---------- memory ---------- */

static void jig_oom(void) __attribute__((noreturn, cold));
static void jig_oom(void) {
    fputs("out of memory\n", stderr);
    _exit(102);
}

#ifdef JIG_POOL
/* Release builds: blocks up to POOL_MAX bytes come from per-thread free lists, one per
 * 16-byte size class, carved from 64 KiB slabs that are never returned. Each block starts
 * with an 8-byte header holding its class (0 for a large block from malloc), so jig_free
 * needs no size. Debug builds use malloc directly so Valgrind sees every block. */
#define POOL_MAX 512
typedef struct pool_block {
    struct pool_block *next;
} pool_block;
static __thread pool_block *pool_free[POOL_MAX / 16 + 1];
static __thread char *pool_cur, *pool_end;

static uint64_t *pool_carve(size_t sz) __attribute__((noinline));
static uint64_t *pool_carve(size_t sz) {
    if ((size_t)(pool_end - pool_cur) < sz) {
        size_t slab = (size_t)1 << 16;
        pool_cur = malloc(slab);
        if (!pool_cur) jig_oom();
        pool_end = pool_cur + slab;
    }
    uint64_t *h = (uint64_t *)pool_cur;
    pool_cur += sz;
    return h;
}

void *jig_alloc(size_t n) {
    size_t need = n + 8;
    if (__builtin_expect(need <= POOL_MAX, 1)) {
        size_t c = (need + 15) >> 4;
        pool_block *b = pool_free[c];
        uint64_t *h;
        if (b) {
            pool_free[c] = b->next;
            h = (uint64_t *)b;
        } else {
            h = pool_carve(c << 4);
        }
        h[0] = c;
        return h + 1;
    }
    uint64_t *h = malloc(need);
    if (!h) jig_oom();
    h[0] = 0;
    return h + 1;
}

void *jig_alloc_obj(size_t n) {
    if (__builtin_expect(n > POOL_MAX, 0)) {
        void *p = malloc(n);
        if (!p) jig_oom();
        return p;
    }
    size_t c = (n + 15) >> 4;
    pool_block *b = pool_free[c];
    if (b) {
        pool_free[c] = b->next;
        return b;
    }
    return pool_carve(c << 4);
}

void jig_free_obj(void *p, size_t n) {
    if (n > POOL_MAX) {
        free(p);
        return;
    }
    size_t c = (n + 15) >> 4;
    pool_block *b = p;
    b->next = pool_free[c];
    pool_free[c] = b;
}

static void *jig_alloc_zeroed(size_t n) {
    if (n + 8 <= POOL_MAX) return memset(jig_alloc(n), 0, n);
    uint64_t *h = calloc(1, n + 8);
    if (!h) jig_oom();
    return h + 1;
}

void jig_free(void *p) {
    if (!p) return;
    uint64_t *h = (uint64_t *)p - 1;
    size_t c = h[0];
    if (c == 0) {
        free(h);
        return;
    }
    pool_block *b = (pool_block *)h;
    b->next = pool_free[c];
    pool_free[c] = b;
}

/* The bytes jig_alloc(n) really provides: its size class, less the header. */
static size_t jig_alloc_size(size_t n) {
    size_t need = n + 8;
    return need <= POOL_MAX ? ((need + 15) & ~(size_t)15) - 8 : n;
}

static void *jig_realloc(void *p, size_t n) {
    if (!p) return jig_alloc(n);
    uint64_t *h = (uint64_t *)p - 1;
    size_t c = h[0];
    if (c == 0) {
        h = realloc(h, n + 8);
        if (!h) jig_oom();
        return h + 1;
    }
    size_t have = (c << 4) - 8;
    if (n <= have) return p;
    void *q = jig_alloc(n);
    memcpy(q, p, have);
    jig_free(p);
    return q;
}
#else
void *jig_alloc(size_t n) {
    void *p = malloc(n ? n : 1);
    if (!p) jig_oom();
    return p;
}

static void *jig_alloc_zeroed(size_t n) {
    void *p = calloc(1, n ? n : 1);
    if (!p) jig_oom();
    return p;
}

void jig_free(void *p) { free(p); }

void *jig_alloc_obj(size_t n) { return jig_alloc(n); }

void jig_free_obj(void *p, size_t n) {
    (void)n;
    free(p);
}

static size_t jig_alloc_size(size_t n) { return n; }

static void *jig_realloc(void *p, size_t n) {
    void *q = realloc(p, n ? n : 1);
    if (!q) jig_oom();
    return q;
}
#endif

/* ---------- tasks ---------- */

/* A batch of `n` independent calls body(ctx, i). The thread that starts it works through it
 * and idle workers help; the starter waits only for indices other threads have taken, so a
 * task that starts its own batch (nesting) always makes progress. Workers are started once
 * and kept, so their allocation pools are never lost. */
typedef struct jig_batch {
    int64_t n, next, helpers;
    void (*body)(void *ctx, int64_t i);
    void *ctx;
    struct jig_batch *link;
} jig_batch;

static pthread_mutex_t batch_mu = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t batch_work = PTHREAD_COND_INITIALIZER;
static pthread_cond_t batch_idle = PTHREAD_COND_INITIALIZER;
static jig_batch *batches;
static int workers = -1;

static void batch_run(jig_batch *b) {
    for (;;) {
        int64_t i = __atomic_fetch_add(&b->next, 1, __ATOMIC_RELAXED);
        if (i >= b->n) return;
        b->body(b->ctx, i);
    }
}

static void *batch_worker(void *arg) {
    (void)arg;
    pthread_mutex_lock(&batch_mu);
    for (;;) {
        jig_batch *b = batches;
        while (b && __atomic_load_n(&b->next, __ATOMIC_RELAXED) >= b->n) b = b->link;
        if (!b) {
            pthread_cond_wait(&batch_work, &batch_mu);
            continue;
        }
        b->helpers++;
        pthread_mutex_unlock(&batch_mu);
        batch_run(b);
        pthread_mutex_lock(&batch_mu);
        if (--b->helpers == 0) pthread_cond_broadcast(&batch_idle);
    }
    return NULL;
}

static int worker_count(void) {
    const char *e = getenv("JIG_THREADS");
    long n = e ? atol(e) : sysconf(_SC_NPROCESSORS_ONLN);
    return n > 1 ? (int)(n - 1) : 0;
}

/* Under batch_mu. */
static void start_workers(void) {
    if (workers >= 0) return;
    workers = worker_count();
    pthread_attr_t attr;
    pthread_attr_init(&attr);
    pthread_attr_setstacksize(&attr, (size_t)256 << 20);
    pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
    for (int i = 0; i < workers; i++) {
        pthread_t t;
        if (pthread_create(&t, &attr, batch_worker, NULL) != 0) {
            workers = i;
            break;
        }
    }
    pthread_attr_destroy(&attr);
}

static void parallel_for(int64_t n, void (*body)(void *, int64_t), void *ctx) {
    pthread_mutex_lock(&batch_mu);
    start_workers();
    if (n < 2 || workers == 0) {
        pthread_mutex_unlock(&batch_mu);
        for (int64_t i = 0; i < n; i++) body(ctx, i);
        return;
    }
    jig_batch b = {n, 0, 0, body, ctx, batches};
    batches = &b;
    pthread_cond_broadcast(&batch_work);
    pthread_mutex_unlock(&batch_mu);
    batch_run(&b);
    pthread_mutex_lock(&batch_mu);
    jig_batch **pp = &batches;
    while (*pp != &b) pp = &(*pp)->link;
    *pp = b.link;
    while (b.helpers > 0) pthread_cond_wait(&batch_idle, &batch_mu);
    pthread_mutex_unlock(&batch_mu);
}

typedef struct {
    jig_fn *fns;
    const char *in;
    size_t isz;
    char *out;
    size_t osz;
    jig_task_call call;
} task_ctx;

static void task_all_body(void *c, int64_t i) {
    task_ctx *t = c;
    t->call(&t->fns[i], NULL, t->out + (size_t)i * t->osz);
}

static void task_map_body(void *c, int64_t i) {
    task_ctx *t = c;
    t->call(t->fns, t->in + (size_t)i * t->isz, t->out + (size_t)i * t->osz);
}

static jig_list *task_results(int64_t n, const jig_type *rt) {
    jig_list *r = jig_list_with_capacity(n, rt);
    r->len = n;
    return r;
}

jig_list *jig_task_all(const jig_list *jobs, jig_task_call call, const jig_type *rt) {
    int64_t n = jig_list_len(jobs);
    if (!n) return NULL;
    jig_list *r = task_results(n, rt);
    task_ctx c = {(jig_fn *)jobs->data, NULL, 0, r->data, rt->size, call};
    parallel_for(n, task_all_body, &c);
    return r;
}

jig_list *jig_task_map(const jig_list *items, jig_fn *f, jig_task_call call, const jig_type *it, const jig_type *rt) {
    int64_t n = jig_list_len(items);
    if (!n) return NULL;
    jig_list *r = task_results(n, rt);
    task_ctx c = {f, items->data, it->size, r->data, rt->size, call};
    parallel_for(n, task_map_body, &c);
    return r;
}

typedef struct {
    jig_fn *f[2];
    jig_task_call call[2];
    void *out[2];
} both_ctx;

static void task_both_body(void *c, int64_t i) {
    both_ctx *b = c;
    b->call[i](b->f[i], NULL, b->out[i]);
}

void jig_task_both(jig_fn *a, jig_fn *b, jig_task_call ca, jig_task_call cb, void *oa, void *ob) {
    both_ctx c = {{a, b}, {ca, cb}, {oa, ob}};
    parallel_for(2, task_both_body, &c);
}

/* ---------- buffers ---------- */

void jig_buf_push(jig_buf *b, const char *s, int64_t n) {
    if (b->len + n > b->cap) {
        int64_t cap = b->cap ? b->cap * 2 : 64;
        while (cap < b->len + n) cap *= 2;
        b->data = jig_realloc(b->data, (size_t)cap);
        b->cap = cap;
    }
    memcpy(b->data + b->len, s, (size_t)n);
    b->len += n;
}

void jig_buf_push_cstr(jig_buf *b, const char *s) { jig_buf_push(b, s, (int64_t)strlen(s)); }

void jig_buf_push_str(jig_buf *b, const jig_str *s) { jig_buf_push(b, jig_str_data(s), jig_str_len(s)); }

void jig_buf_free(jig_buf *b) {
    jig_free(b->data);
    b->data = NULL;
    b->len = b->cap = 0;
}

jig_str *jig_buf_to_str(jig_buf *b) {
    jig_str *s = jig_str_new(b->data, b->len);
    jig_buf_free(b);
    return s;
}

/* ---------- strings ---------- */

/* A string block with room for at least `len` bytes; its capacity is all the block holds, so
 * appends can use the allocator's rounding. */
static jig_str *jig_str_block(int64_t len) {
    size_t sz = jig_alloc_size(sizeof(jig_str) + (size_t)len + 1);
    jig_str *s = jig_alloc(sz);
    s->rc = 1;
    s->cap = (int64_t)(sz - sizeof(jig_str) - 1);
    return s;
}

jig_str *jig_str_new(const char *data, int64_t len) {
    if (len == 0) return NULL;
    jig_str *s = jig_str_block(len);
    s->len = len;
    memcpy(s->data, data, (size_t)len);
    s->data[len] = 0;
    return s;
}

jig_str *jig_str_concat(const jig_str *a, const jig_str *b) {
    int64_t la = jig_str_len(a), lb = jig_str_len(b);
    if (la + lb == 0) return NULL;
    jig_str *s = jig_str_block(la + lb);
    s->len = la + lb;
    memcpy(s->data, jig_str_data(a), (size_t)la);
    memcpy(s->data + la, jig_str_data(b), (size_t)lb);
    s->data[la + lb] = 0;
    return s;
}

jig_str *jig_str_concat_n(int64_t n, const jig_str *const *xs) {
    int64_t len = 0;
    for (int64_t i = 0; i < n; i++) len += jig_str_len(xs[i]);
    if (len == 0) return NULL;
    jig_str *s = jig_str_block(len);
    s->len = len;
    char *d = s->data;
    for (int64_t i = 0; i < n; i++) {
        int64_t k = jig_str_len(xs[i]);
        jig_copy_small(d, jig_str_data(xs[i]), (size_t)k);
        d += k;
    }
    *d = 0;
    return s;
}

void jig_str_append_slow(jig_str **sp, const jig_str *t) {
    int64_t lt = jig_str_len(t);
    if (lt == 0) return;
    jig_str *s = *sp;
    int64_t ls = jig_str_len(s);
    if (s && s->rc == 1 && s != t) {
        if (ls + lt > s->cap) {
            /* Grow geometrically so a loop of appends is linear. */
            int64_t cap = s->cap * 2 > ls + lt ? s->cap * 2 : ls + lt;
            size_t sz = jig_alloc_size(sizeof(jig_str) + (size_t)cap + 1);
            s = jig_realloc(s, sz);
            s->cap = (int64_t)(sz - sizeof(jig_str) - 1);
        }
        memcpy(s->data + ls, t->data, (size_t)lt);
        s->len = ls + lt;
        s->data[s->len] = 0;
        *sp = s;
        return;
    }
    /* Shared, static, empty or appending to itself: a fresh string with room to grow. */
    jig_str *n = jig_str_block(ls + lt < 16 ? 16 : ls + lt);
    n->len = ls + lt;
    memcpy(n->data, jig_str_data(s), (size_t)ls);
    memcpy(n->data + ls, jig_str_data(t), (size_t)lt);
    n->data[n->len] = 0;
    jig_str_release(s);
    *sp = n;
}

bool jig_str_eq(const jig_str *a, const jig_str *b) {
    int64_t la = jig_str_len(a);
    return la == jig_str_len(b) && memcmp(jig_str_data(a), jig_str_data(b), (size_t)la) == 0;
}

int jig_str_cmp(const jig_str *a, const jig_str *b) {
    int64_t la = jig_str_len(a), lb = jig_str_len(b);
    int c = memcmp(jig_str_data(a), jig_str_data(b), (size_t)(la < lb ? la : lb));
    if (c) return c < 0 ? -1 : 1;
    return la < lb ? -1 : la > lb ? 1 : 0;
}

uint64_t jig_hash_u64(uint64_t x) {
    x ^= x >> 30;
    x *= 0xbf58476d1ce4e5b9ull;
    x ^= x >> 27;
    x *= 0x94d049bb133111ebull;
    return x ^ (x >> 31);
}

uint64_t jig_hash_combine(uint64_t h, uint64_t x) { return jig_hash_u64(h ^ (x + 0x9e3779b97f4a7c15ull + (h << 6))); }

/* Eight bytes at a time; the tail is read with overlapping loads inside the string. */
uint64_t jig_str_hash(const jig_str *s) {
    const unsigned char *p = (const unsigned char *)jig_str_data(s);
    int64_t n = jig_str_len(s);
    uint64_t h = 0x9e3779b97f4a7c15ull ^ (uint64_t)n;
    for (; n > 8; p += 8, n -= 8) {
        uint64_t w;
        memcpy(&w, p, 8);
        h = (h ^ w) * 0xbf58476d1ce4e5b9ull;
        h ^= h >> 29;
    }
    uint64_t w = 0;
    if (n == 8) {
        memcpy(&w, p, 8);
    } else if (n >= 4) {
        uint32_t a, b;
        memcpy(&a, p, 4);
        memcpy(&b, p + n - 4, 4);
        w = ((uint64_t)a << 32) | b;
    } else if (n) {
        w = (uint64_t)p[0] | (uint64_t)p[n / 2] << 8 | (uint64_t)p[n - 1] << 16;
    }
    return jig_hash_u64(h ^ w);
}

uint8_t jig_str_byte(const jig_str *s, int64_t i, const char *loc) {
    if (i < 0 || i >= jig_str_len(s)) jig_index_oob(i, jig_str_len(s), loc);
    return (uint8_t)s->data[i];
}

jig_str *jig_str_slice(const jig_str *s, int64_t lo, int64_t hi, const char *loc) {
    int64_t n = jig_str_len(s);
    if (lo < 0 || hi < lo || hi > n) {
        char msg[160];
        snprintf(msg, sizeof msg, "slice %lld..%lld is out of bounds for a string of length %lld", (long long)lo, (long long)hi, (long long)n);
        jig_panic(msg, loc);
    }
    const char *d = jig_str_data(s);
    /* character boundaries: a continuation byte cannot start or end a slice */
    if ((lo < n && ((unsigned char)d[lo] & 0xc0) == 0x80) || (hi < n && ((unsigned char)d[hi] & 0xc0) == 0x80)) {
        char msg[160];
        snprintf(msg, sizeof msg, "slice %lld..%lld does not fall on character boundaries", (long long)lo, (long long)hi);
        jig_panic(msg, loc);
    }
    return jig_str_new(d + lo, hi - lo);
}

int64_t jig_str_find(const jig_str *s, const jig_str *t) {
    int64_t n = jig_str_len(s), m = jig_str_len(t);
    if (m == 0) return 0;
    const char *a = jig_str_data(s), *b = jig_str_data(t);
    const char *p = m <= n ? memmem(a, (size_t)n, b, (size_t)m) : NULL;
    return p ? p - a : -1;
}

bool jig_str_contains(const jig_str *s, const jig_str *t) { return jig_str_find(s, t) >= 0; }

bool jig_str_starts_with(const jig_str *s, const jig_str *t) {
    int64_t m = jig_str_len(t);
    return m <= jig_str_len(s) && memcmp(jig_str_data(s), jig_str_data(t), (size_t)m) == 0;
}

bool jig_str_ends_with(const jig_str *s, const jig_str *t) {
    int64_t n = jig_str_len(s), m = jig_str_len(t);
    return m <= n && memcmp(jig_str_data(s) + n - m, jig_str_data(t), (size_t)m) == 0;
}

static void push_str_elem(jig_list **l, const char *d, int64_t n) {
    jig_str *p = jig_str_new(d, n);
    jig_list_push_fast(l, &p, sizeof p, &jig_type_str);
}

static int utf8_len(unsigned char c) { return c < 0x80 ? 1 : c < 0xe0 ? 2 : c < 0xf0 ? 3 : 4; }

jig_list *jig_str_split(const jig_str *s, const jig_str *sep) {
    jig_list *out = NULL;
    const char *d = jig_str_data(s);
    int64_t n = jig_str_len(s), m = jig_str_len(sep);
    if (m == 0) {
        for (int64_t i = 0; i < n;) {
            int k = utf8_len((unsigned char)d[i]);
            push_str_elem(&out, d + i, k);
            i += k;
        }
        return out;
    }
    const char *sd = jig_str_data(sep);
    int64_t start = 0;
    for (int64_t i = 0; i + m <= n;) {
        /* Jump to the next occurrence of the separator's first byte. */
        const char *p = memchr(d + i, sd[0], (size_t)(n - m + 1 - i));
        if (!p) break;
        i = p - d;
        if (m == 1 || memcmp(p + 1, sd + 1, (size_t)m - 1) == 0) {
            push_str_elem(&out, d + start, i - start);
            i += m;
            start = i;
        } else {
            i++;
        }
    }
    push_str_elem(&out, d + start, n - start);
    return out;
}

jig_str *jig_str_split_next(const jig_str *s, const jig_str *sep, int64_t *pos) {
    const char *d = jig_str_data(s), *sd = jig_str_data(sep);
    int64_t n = jig_str_len(s), m = jig_str_len(sep), i = *pos;
    const char *p = i + m <= n ? memmem(d + i, (size_t)(n - i), sd, (size_t)m) : NULL;
    if (!p) {
        *pos = -1;
        return jig_str_new(d + i, n - i);
    }
    *pos = (p - d) + m;
    return jig_str_new(d + i, (p - d) - i);
}

jig_list *jig_str_lines(const jig_str *s) {
    jig_list *out = NULL;
    const char *d = jig_str_data(s);
    int64_t n = jig_str_len(s), start = 0;
    for (int64_t i = 0; i < n; i++) {
        if (d[i] == '\n') {
            int64_t end = i;
            if (end > start && d[end - 1] == '\r') end--;
            push_str_elem(&out, d + start, end - start);
            start = i + 1;
        }
    }
    if (start < n) {
        int64_t end = n;
        if (end > start && d[end - 1] == '\r') end--;
        push_str_elem(&out, d + start, end - start);
    }
    return out;
}

static bool is_ws(unsigned char c) { return c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\v' || c == '\f'; }

jig_str *jig_str_trim(const jig_str *s) {
    const char *d = jig_str_data(s);
    int64_t lo = 0, hi = jig_str_len(s);
    while (lo < hi && is_ws((unsigned char)d[lo])) lo++;
    while (hi > lo && is_ws((unsigned char)d[hi - 1])) hi--;
    return jig_str_new(d + lo, hi - lo);
}

jig_str *jig_str_replace(const jig_str *s, const jig_str *from, const jig_str *to) {
    int64_t m = jig_str_len(from);
    if (m == 0) {
        jig_str *r = (jig_str *)s;
        jig_str_retain(r);
        return r;
    }
    jig_buf b = {0};
    const char *d = jig_str_data(s);
    int64_t n = jig_str_len(s);
    for (int64_t i = 0; i < n;) {
        if (i + m <= n && memcmp(d + i, jig_str_data(from), (size_t)m) == 0) {
            jig_buf_push_str(&b, to);
            i += m;
        } else {
            jig_buf_push(&b, d + i, 1);
            i++;
        }
    }
    return jig_buf_to_str(&b);
}

jig_str *jig_str_repeat(const jig_str *s, int64_t n, const char *loc) {
    if (n < 0) jig_panic("repeat count is negative", loc);
    jig_buf b = {0};
    for (int64_t i = 0; i < n; i++) jig_buf_push_str(&b, s);
    return jig_buf_to_str(&b);
}

static uint32_t decode_utf8(const char *d, int64_t *i, int64_t n) {
    unsigned char c = (unsigned char)d[*i];
    int k = utf8_len(c);
    uint32_t cp = k == 1 ? c : k == 2 ? (c & 0x1f) : k == 3 ? (c & 0x0f) : (c & 0x07);
    for (int j = 1; j < k && *i + j < n; j++) cp = (cp << 6) | ((unsigned char)d[*i + j] & 0x3f);
    *i += k;
    return cp;
}

static void encode_utf8(jig_buf *b, uint32_t c) {
    char tmp[4];
    int n;
    if (c < 0x80) {
        tmp[0] = (char)c;
        n = 1;
    } else if (c < 0x800) {
        tmp[0] = (char)(0xc0 | (c >> 6));
        tmp[1] = (char)(0x80 | (c & 0x3f));
        n = 2;
    } else if (c < 0x10000) {
        tmp[0] = (char)(0xe0 | (c >> 12));
        tmp[1] = (char)(0x80 | ((c >> 6) & 0x3f));
        tmp[2] = (char)(0x80 | (c & 0x3f));
        n = 3;
    } else {
        tmp[0] = (char)(0xf0 | (c >> 18));
        tmp[1] = (char)(0x80 | ((c >> 12) & 0x3f));
        tmp[2] = (char)(0x80 | ((c >> 6) & 0x3f));
        tmp[3] = (char)(0x80 | (c & 0x3f));
        n = 4;
    }
    jig_buf_push(b, tmp, n);
}

static jig_str *map_chars(const jig_str *s, uint32_t (*f)(uint32_t)) {
    jig_buf b = {0};
    const char *d = jig_str_data(s);
    int64_t n = jig_str_len(s);
    for (int64_t i = 0; i < n;) encode_utf8(&b, f(decode_utf8(d, &i, n)));
    return jig_buf_to_str(&b);
}

jig_str *jig_str_to_upper(const jig_str *s) { return map_chars(s, jig_char_to_upper); }
jig_str *jig_str_to_lower(const jig_str *s) { return map_chars(s, jig_char_to_lower); }

bool jig_str_to_int(const jig_str *s, int64_t *out) {
    jig_str *t = jig_str_trim(s);
    const char *d = jig_str_data(t);
    int64_t n = jig_str_len(t);
    bool ok = n > 0;
    int64_t i = 0;
    bool neg = false;
    if (ok && (d[0] == '-' || d[0] == '+')) {
        neg = d[0] == '-';
        i = 1;
        ok = n > 1;
    }
    uint64_t v = 0;
    for (; ok && i < n; i++) {
        if (d[i] < '0' || d[i] > '9') {
            ok = false;
            break;
        }
        uint64_t nv = v * 10 + (uint64_t)(d[i] - '0');
        if (nv / 10 != v || nv > (uint64_t)INT64_MAX + (neg ? 1 : 0)) {
            ok = false;
            break;
        }
        v = nv;
    }
    if (ok) *out = neg ? (int64_t)(0 - v) : (int64_t)v;
    jig_str_release(t);
    return ok;
}

bool jig_str_to_float(const jig_str *s, double *out) {
    jig_str *t = jig_str_trim(s);
    bool ok = false;
    if (jig_str_len(t) > 0) {
        char *end;
        errno = 0;
        double v = strtod(t->data, &end);
        ok = *end == 0 && end != t->data;
        if (ok) *out = v;
    }
    jig_str_release(t);
    return ok;
}

jig_list *jig_str_bytes(const jig_str *s) {
    int64_t n = jig_str_len(s);
    jig_list *l = jig_list_with_capacity(n, &jig_type_u8);
    if (n) {
        memcpy(l->data, jig_str_data(s), (size_t)n);
        l->len = n;
    }
    return l;
}

jig_list *jig_str_chars(const jig_str *s) {
    jig_list *out = NULL;
    const char *d = jig_str_data(s);
    int64_t n = jig_str_len(s);
    for (int64_t i = 0; i < n;) {
        uint32_t c = decode_utf8(d, &i, n);
        jig_list_push(&out, &c, &jig_type_char);
    }
    return out;
}

jig_str *jig_str_from_chars(const jig_list *cs) {
    jig_buf b = {0};
    for (int64_t i = 0; i < jig_list_len(cs); i++) encode_utf8(&b, ((uint32_t *)cs->data)[i]);
    return jig_buf_to_str(&b);
}

jig_str *jig_str_from_bytes(const jig_list *bs, bool *ok) {
    int64_t n = jig_list_len(bs);
    const unsigned char *d = n ? (const unsigned char *)bs->data : (const unsigned char *)"";
    /* validate UTF-8 */
    for (int64_t i = 0; i < n;) {
        int k = utf8_len(d[i]);
        if ((d[i] & 0xc0) == 0x80 || i + k > n) {
            *ok = false;
            return NULL;
        }
        for (int j = 1; j < k; j++)
            if ((d[i + j] & 0xc0) != 0x80) {
                *ok = false;
                return NULL;
            }
        i += k;
    }
    *ok = true;
    return jig_str_new((const char *)d, n);
}

jig_str *jig_char_to_str(uint32_t c) {
    jig_buf b = {0};
    encode_utf8(&b, c);
    return jig_buf_to_str(&b);
}

/* ---------- lists ---------- */

void jig_list_retain(jig_list *l) {
    if (l) jig_rc_inc(&l->rc);
}

void jig_list_release(jig_list *l, const jig_type *t) {
    if (!l || !jig_rc_dec(&l->rc)) return;
    if (t->release)
        for (int64_t i = 0; i < l->len; i++) t->release(l->data + (size_t)i * t->size);
    jig_free(l->data);
    jig_free_obj(l, sizeof(jig_list));
}

jig_list *jig_list_with_capacity(int64_t n, const jig_type *t) {
    jig_list *l = jig_alloc_obj(sizeof(jig_list));
    l->rc = 1;
    l->len = 0;
    l->cap = n;
    l->data = n ? jig_alloc((size_t)n * t->size) : NULL;
    return l;
}

void jig_list_make_unique(jig_list **lp, const jig_type *t) {
    jig_list *l = *lp;
    if (!l) {
        *lp = jig_list_with_capacity(4, t);
        return;
    }
    if (l->rc == 1) return;
    jig_list *c = jig_list_with_capacity(l->len > 4 ? l->len : 4, t);
    if (l->len) memcpy(c->data, l->data, (size_t)l->len * t->size);
    c->len = l->len;
    if (t->retain)
        for (int64_t i = 0; i < c->len; i++) t->retain(c->data + (size_t)i * t->size);
    jig_list_release(l, t);
    *lp = c;
}

static void grow(jig_list *l, int64_t need, const jig_type *t) {
    if (need <= l->cap) return;
    int64_t cap = l->cap ? l->cap * 2 : 4;
    while (cap < need) cap *= 2;
    l->data = jig_realloc(l->data, (size_t)cap * t->size);
    l->cap = cap;
}

void *jig_list_at(const jig_list *l, int64_t i, const jig_type *t, const char *loc) {
    int64_t n = jig_list_len(l);
    if (i < 0 || i >= n) jig_index_oob(i, n, loc);
    return l->data + (size_t)i * t->size;
}

void *jig_list_at_mut(jig_list **l, int64_t i, const jig_type *t, const char *loc) {
    int64_t n = jig_list_len(*l);
    if (i < 0 || i >= n) jig_index_oob(i, n, loc);
    jig_list_make_unique(l, t);
    return (*l)->data + (size_t)i * t->size;
}

void jig_list_push(jig_list **lp, void *elem, const jig_type *t) {
    jig_list_make_unique(lp, t);
    jig_list *l = *lp;
    grow(l, l->len + 1, t);
    memcpy(l->data + (size_t)l->len * t->size, elem, t->size);
    l->len++;
}

bool jig_list_pop(jig_list **lp, void *out, const jig_type *t) {
    if (jig_list_len(*lp) == 0) return false;
    jig_list_make_unique(lp, t);
    jig_list *l = *lp;
    l->len--;
    memcpy(out, l->data + (size_t)l->len * t->size, t->size);
    return true;
}

bool jig_list_get(const jig_list *l, int64_t i, void *out, const jig_type *t) {
    if (i < 0 || i >= jig_list_len(l)) return false;
    memcpy(out, l->data + (size_t)i * t->size, t->size);
    if (t->retain) t->retain(out);
    return true;
}

void jig_list_insert(jig_list **lp, int64_t i, void *elem, const jig_type *t, const char *loc) {
    int64_t n = jig_list_len(*lp);
    if (i < 0 || i > n) {
        char msg[128];
        snprintf(msg, sizeof msg, "insert index %lld is out of bounds for length %lld", (long long)i, (long long)n);
        jig_panic(msg, loc);
    }
    jig_list_make_unique(lp, t);
    jig_list *l = *lp;
    grow(l, n + 1, t);
    memmove(l->data + (size_t)(i + 1) * t->size, l->data + (size_t)i * t->size, (size_t)(n - i) * t->size);
    memcpy(l->data + (size_t)i * t->size, elem, t->size);
    l->len++;
}

void jig_list_remove(jig_list **lp, int64_t i, void *out, const jig_type *t, const char *loc) {
    int64_t n = jig_list_len(*lp);
    if (i < 0 || i >= n) {
        char msg[128];
        snprintf(msg, sizeof msg, "remove index %lld is out of bounds for length %lld", (long long)i, (long long)n);
        jig_panic(msg, loc);
    }
    jig_list_make_unique(lp, t);
    jig_list *l = *lp;
    memcpy(out, l->data + (size_t)i * t->size, t->size);
    memmove(l->data + (size_t)i * t->size, l->data + (size_t)(i + 1) * t->size, (size_t)(n - i - 1) * t->size);
    l->len--;
}

void jig_list_clear(jig_list **lp, const jig_type *t) {
    jig_list_release(*lp, t);
    *lp = NULL;
}

void jig_list_extend(jig_list **lp, const jig_list *other, const jig_type *t) {
    int64_t m = jig_list_len(other);
    if (!m) return;
    jig_list_make_unique(lp, t);
    jig_list *l = *lp;
    grow(l, l->len + m, t);
    memcpy(l->data + (size_t)l->len * t->size, other->data, (size_t)m * t->size);
    if (t->retain)
        for (int64_t i = 0; i < m; i++) t->retain(l->data + (size_t)(l->len + i) * t->size);
    l->len += m;
}

jig_list *jig_list_repeat(const jig_list *l, int64_t n, const jig_type *t, const char *loc) {
    if (n < 0) jig_panic("repeat count is negative", loc);
    int64_t m = jig_list_len(l);
    if (!m || !n) return NULL;
    if (m > INT64_MAX / n || (uint64_t)(m * n) > SIZE_MAX / t->size) jig_panic("the repeated list is too long", loc);
    size_t chunk = (size_t)m * t->size, all = (size_t)(m * n) * t->size;
    bool zero = true;
    for (size_t i = 0; i < chunk && zero; i++) zero = l->data[i] == 0;
    jig_list *r = jig_alloc_obj(sizeof(jig_list));
    r->rc = 1;
    r->len = r->cap = m * n;
    if (zero) {
        /* All-zero elements (false, 0, empty values): fresh zeroed pages, no copying. */
        r->data = jig_alloc_zeroed(all);
        return r;
    }
    r->data = jig_alloc(all);
    memcpy(r->data, l->data, chunk);
    for (size_t done = chunk; done < all;) {
        size_t c = done < all - done ? done : all - done;
        memcpy(r->data + done, r->data, c);
        done += c;
    }
    if (t->retain)
        for (int64_t i = 0; i < r->len; i++) t->retain(r->data + (size_t)i * t->size);
    return r;
}

void jig_list_reverse(jig_list **lp, const jig_type *t) {
    if (jig_list_len(*lp) < 2) return;
    jig_list_make_unique(lp, t);
    jig_list *l = *lp;
    char *tmp = jig_alloc(t->size);
    for (int64_t i = 0, j = l->len - 1; i < j; i++, j--) {
        memcpy(tmp, l->data + (size_t)i * t->size, t->size);
        memcpy(l->data + (size_t)i * t->size, l->data + (size_t)j * t->size, t->size);
        memcpy(l->data + (size_t)j * t->size, tmp, t->size);
    }
    jig_free(tmp);
}

void jig_list_swap(jig_list **lp, int64_t i, int64_t j, const jig_type *t, const char *loc) {
    int64_t n = jig_list_len(*lp);
    if (i < 0 || j < 0 || i >= n || j >= n) {
        char msg[128];
        snprintf(msg, sizeof msg, "swap(%lld, %lld) is out of bounds for length %lld", (long long)i, (long long)j, (long long)n);
        jig_panic(msg, loc);
    }
    if (i == j) return;
    jig_list_make_unique(lp, t);
    jig_list *l = *lp;
    char small[64];
    char *tmp = t->size <= sizeof small ? small : jig_alloc(t->size);
    memcpy(tmp, l->data + (size_t)i * t->size, t->size);
    memcpy(l->data + (size_t)i * t->size, l->data + (size_t)j * t->size, t->size);
    memcpy(l->data + (size_t)j * t->size, tmp, t->size);
    jig_free(tmp);
}

void jig_list_truncate(jig_list **lp, int64_t n, const jig_type *t, const char *loc) {
    if (n < 0) jig_panic("truncate length is negative", loc);
    if (n >= jig_list_len(*lp)) return;
    jig_list_make_unique(lp, t);
    jig_list *l = *lp;
    if (t->release)
        for (int64_t i = n; i < l->len; i++) t->release(l->data + (size_t)i * t->size);
    l->len = n;
}

typedef struct sort_ctx {
    const jig_type *t;
    bool (*less)(jig_fn *, const void *, const void *);
    jig_fn *f;
} sort_ctx;

static bool sort_less(const sort_ctx *c, const void *a, const void *b) {
    if (c->less) return c->less(c->f, a, b);
    return c->t->cmp(a, b) < 0;
}

static void merge_sort(char *xs, char *buf, int64_t n, const sort_ctx *c) {
    size_t sz = c->t->size;
    if (n < 2) return;
    int64_t mid = n / 2;
    merge_sort(xs, buf, mid, c);
    merge_sort(xs + (size_t)mid * sz, buf, n - mid, c);
    int64_t i = 0, j = mid, k = 0;
    while (i < mid && j < n) {
        if (sort_less(c, xs + (size_t)j * sz, xs + (size_t)i * sz)) {
            memcpy(buf + (size_t)k * sz, xs + (size_t)j * sz, sz);
            j++;
        } else {
            memcpy(buf + (size_t)k * sz, xs + (size_t)i * sz, sz);
            i++;
        }
        k++;
    }
    while (i < mid) memcpy(buf + (size_t)(k++) * sz, xs + (size_t)(i++) * sz, sz);
    while (j < n) memcpy(buf + (size_t)(k++) * sz, xs + (size_t)(j++) * sz, sz);
    memcpy(xs, buf, (size_t)n * sz);
}

static void sort_impl(jig_list **lp, const sort_ctx *c) {
    if (jig_list_len(*lp) < 2) return;
    jig_list_make_unique(lp, c->t);
    jig_list *l = *lp;
    char *buf = jig_alloc((size_t)l->len * c->t->size);
    merge_sort(l->data, buf, l->len, c);
    jig_free(buf);
}

void jig_swap_oob(int64_t i, int64_t j, int64_t n, const char *loc) {
    char msg[128];
    snprintf(msg, sizeof msg, "swap(%lld, %lld) is out of bounds for length %lld", (long long)i, (long long)j, (long long)n);
    jig_panic(msg, loc);
}

/* Sorting primitive keys: equal keys are indistinguishable, so stability doesn't matter.
 * Introsort with a branchless Lomuto partition (no mispredicted branches on random data),
 * median-of-3 (ninther for large ranges) pivots, a second pass that gathers keys equal to
 * a minimal pivot (so many duplicates stay linear), heapsort past the depth limit and
 * insertion sort for short ranges. Instantiated per scalar type. */
#define JIG_SORT_IMPL(NAME, T)                                                                  \
    static void NAME##_sift(T *a, int64_t root, int64_t n) {                                    \
        for (;;) {                                                                              \
            int64_t c = 2 * root + 1;                                                           \
            if (c >= n) return;                                                                 \
            if (c + 1 < n && a[c] < a[c + 1]) c++;                                              \
            if (!(a[root] < a[c])) return;                                                      \
            T t = a[root]; a[root] = a[c]; a[c] = t;                                            \
            root = c;                                                                           \
        }                                                                                       \
    }                                                                                           \
    static inline int64_t NAME##_med3(T *a, int64_t i, int64_t j, int64_t k) {                  \
        return a[i] < a[j] ? (a[j] < a[k] ? j : (a[i] < a[k] ? k : i))                          \
                           : (a[i] < a[k] ? i : (a[j] < a[k] ? k : j));                         \
    }                                                                                           \
    static void NAME##_rec(T *a, int64_t n, int depth) {                                        \
        while (n > 24) {                                                                        \
            if (depth-- == 0) {                                                                 \
                for (int64_t i = n / 2 - 1; i >= 0; i--) NAME##_sift(a, i, n);                  \
                for (int64_t e = n - 1; e > 0; e--) {                                           \
                    T t = a[0]; a[0] = a[e]; a[e] = t;                                          \
                    NAME##_sift(a, 0, e);                                                       \
                }                                                                               \
                return;                                                                         \
            }                                                                                   \
            int64_t m = n / 2, pi;                                                              \
            if (n > 128) {                                                                      \
                int64_t s = n / 8;                                                              \
                pi = NAME##_med3(a, NAME##_med3(a, 0, s, 2 * s), NAME##_med3(a, m - s, m, m + s), \
                                 NAME##_med3(a, n - 1 - 2 * s, n - 1 - s, n - 1));               \
            } else {                                                                            \
                pi = NAME##_med3(a, 0, m, n - 1);                                               \
            }                                                                                   \
            { T t = a[0]; a[0] = a[pi]; a[pi] = t; }                                            \
            T p = a[0];                                                                         \
            int64_t lt = 1;                                                                     \
            for (int64_t i = 1; i < n; i++) {                                                   \
                T v = a[i];                                                                     \
                int less = v < p;                                                               \
                a[i] = a[lt];                                                                   \
                a[lt] = v;                                                                      \
                lt += less;                                                                     \
            }                                                                                   \
            lt--;                                                                               \
            { T t = a[0]; a[0] = a[lt]; a[lt] = t; }                                            \
            /* a[0..lt) < p == a[lt] <= a(lt..n) */                                             \
            int64_t lo = lt + 1;                                                                \
            if (lt == 0) {                                                                      \
                /* The pivot is the minimum: keys equal to it are done. */                      \
                for (int64_t i = 1; i < n; i++) {                                               \
                    T v = a[i];                                                                 \
                    int eq = !(p < v);                                                          \
                    a[i] = a[lo];                                                               \
                    a[lo] = v;                                                                  \
                    lo += eq;                                                                   \
                }                                                                               \
            }                                                                                   \
            /* Recurse into the smaller side, loop on the larger. */                           \
            if (lt < n - lo) {                                                                  \
                NAME##_rec(a, lt, depth);                                                       \
                a += lo;                                                                        \
                n -= lo;                                                                        \
            } else {                                                                            \
                NAME##_rec(a + lo, n - lo, depth);                                              \
                n = lt;                                                                         \
            }                                                                                   \
        }                                                                                       \
        for (int64_t i = 1; i < n; i++) {                                                       \
            T v = a[i];                                                                         \
            int64_t k = i;                                                                      \
            while (k > 0 && v < a[k - 1]) { a[k] = a[k - 1]; k--; }                             \
            a[k] = v;                                                                           \
        }                                                                                       \
    }                                                                                           \
    void NAME(jig_list **lp, const jig_type *t) {                                               \
        int64_t n = jig_list_len(*lp);                                                          \
        if (n < 2) return;                                                                      \
        jig_list_make_unique(lp, t);                                                            \
        int depth = 2;                                                                          \
        for (int64_t m = n; m > 1; m >>= 1) depth += 2;                                         \
        NAME##_rec((T *)(*lp)->data, n, depth);                                                 \
    }
JIG_SORT_IMPL(jig_list_sort_i64, int64_t)
JIG_SORT_IMPL(jig_list_sort_i32, int32_t)
JIG_SORT_IMPL(jig_list_sort_u8, uint8_t)
JIG_SORT_IMPL(jig_list_sort_u32, uint32_t)
JIG_SORT_IMPL(jig_list_sort_u64, uint64_t)

void jig_list_sort(jig_list **lp, const jig_type *t) {
    sort_ctx c = {t, NULL, NULL};
    sort_impl(lp, &c);
}

void jig_list_sort_by(jig_list **lp, const jig_type *t, bool (*less)(jig_fn *, const void *, const void *), jig_fn *f) {
    sort_ctx c = {t, less, f};
    sort_impl(lp, &c);
}

jig_list *jig_list_slice(const jig_list *l, int64_t lo, int64_t hi, const jig_type *t, const char *loc) {
    int64_t n = jig_list_len(l);
    if (lo < 0 || hi < lo || hi > n) {
        char msg[160];
        snprintf(msg, sizeof msg, "slice %lld..%lld is out of bounds for a list of length %lld", (long long)lo, (long long)hi, (long long)n);
        jig_panic(msg, loc);
    }
    if (hi == lo) return NULL;
    jig_list *r = jig_list_with_capacity(hi - lo, t);
    memcpy(r->data, l->data + (size_t)lo * t->size, (size_t)(hi - lo) * t->size);
    r->len = hi - lo;
    if (t->retain)
        for (int64_t i = 0; i < r->len; i++) t->retain(r->data + (size_t)i * t->size);
    return r;
}

bool jig_list_eq(const jig_list *a, const jig_list *b, const jig_type *t) {
    int64_t n = jig_list_len(a);
    if (n != jig_list_len(b)) return false;
    if (a == b) return true;
    for (int64_t i = 0; i < n; i++)
        if (!t->eq(a->data + (size_t)i * t->size, b->data + (size_t)i * t->size)) return false;
    return true;
}

int jig_list_cmp(const jig_list *a, const jig_list *b, const jig_type *t) {
    int64_t na = jig_list_len(a), nb = jig_list_len(b);
    for (int64_t i = 0; i < na && i < nb; i++) {
        int c = t->cmp(a->data + (size_t)i * t->size, b->data + (size_t)i * t->size);
        if (c) return c;
    }
    return na < nb ? -1 : na > nb ? 1 : 0;
}

uint64_t jig_list_hash(const jig_list *a, const jig_type *t) {
    uint64_t h = jig_hash_u64((uint64_t)jig_list_len(a));
    for (int64_t i = 0; i < jig_list_len(a); i++) h = jig_hash_combine(h, t->hash(a->data + (size_t)i * t->size));
    return h;
}

void jig_list_show(jig_buf *b, const jig_list *l, const jig_type *t) {
    jig_buf_push(b, "[", 1);
    for (int64_t i = 0; i < jig_list_len(l); i++) {
        if (i) jig_buf_push(b, ", ", 2);
        t->show(b, l->data + (size_t)i * t->size, true);
    }
    jig_buf_push(b, "]", 1);
}

/* ---------- maps ---------- */

static void map_rebuild_index(jig_map *m, const jig_type *k) {
    int64_t cap = 8;
    while (cap < m->len * 2 + 2) cap *= 2;
    jig_free(m->index);
    m->index = jig_alloc((size_t)cap * sizeof(int32_t));
    for (int64_t i = 0; i < cap; i++) m->index[i] = -1;
    m->index_cap = cap;
    for (int64_t i = 0; i < m->len; i++) {
        uint64_t h = k->hash(m->keys + (size_t)i * k->size);
        int64_t slot = (int64_t)(h & (uint64_t)(cap - 1));
        while (m->index[slot] >= 0) slot = (slot + 1) & (cap - 1);
        m->index[slot] = (int32_t)i;
    }
}

static int64_t map_find(const jig_map *m, const void *key, const jig_type *k) {
    if (!m || !m->len) return -1;
    uint64_t h = k->hash(key);
    int64_t mask = m->index_cap - 1;
    for (int64_t slot = (int64_t)(h & (uint64_t)mask);; slot = (slot + 1) & mask) {
        int32_t idx = m->index[slot];
        if (idx < 0) return -1;
        if (k->eq(m->keys + (size_t)idx * k->size, key)) return idx;
    }
}

void jig_map_retain(jig_map *m) {
    if (m) jig_rc_inc(&m->rc);
}

void jig_map_release(jig_map *m, const jig_type *k, const jig_type *v) {
    if (!m || !jig_rc_dec(&m->rc)) return;
    for (int64_t i = 0; i < m->len; i++) {
        if (k->release) k->release(m->keys + (size_t)i * k->size);
        if (v->release) v->release(m->vals + (size_t)i * v->size);
    }
    jig_free(m->keys);
    jig_free(m->vals);
    jig_free(m->index);
    jig_free_obj(m, sizeof(jig_map));
}

static void map_copy(jig_map **mp, const jig_type *k, const jig_type *v) __attribute__((noinline));

static inline void map_make_unique(jig_map **mp, const jig_type *k, const jig_type *v) {
    if (__builtin_expect(!*mp || (*mp)->rc != 1, 0)) map_copy(mp, k, v);
}

static void map_copy(jig_map **mp, const jig_type *k, const jig_type *v) {
    jig_map *m = *mp;
    jig_map *c = jig_alloc_obj(sizeof(jig_map));
    c->rc = 1;
    c->len = m ? m->len : 0;
    c->cap = c->len > 4 ? c->len : 4;
    c->keys = jig_alloc((size_t)c->cap * k->size);
    c->vals = jig_alloc((size_t)c->cap * v->size);
    c->index = NULL;
    if (m && m->len) {
        memcpy(c->keys, m->keys, (size_t)m->len * k->size);
        memcpy(c->vals, m->vals, (size_t)m->len * v->size);
        for (int64_t i = 0; i < c->len; i++) {
            if (k->retain) k->retain(c->keys + (size_t)i * k->size);
            if (v->retain) v->retain(c->vals + (size_t)i * v->size);
        }
    }
    map_rebuild_index(c, k);
    if (m) jig_map_release(m, k, v);
    *mp = c;
}

void jig_map_insert(jig_map **mp, void *key, void *val, const jig_type *k, const jig_type *v) {
    map_make_unique(mp, k, v);
    jig_map *m = *mp;
    int64_t idx = map_find(m, key, k);
    if (idx >= 0) {
        if (v->release) v->release(m->vals + (size_t)idx * v->size);
        memcpy(m->vals + (size_t)idx * v->size, val, v->size);
        if (k->release) k->release(key);
        return;
    }
    if (m->len == m->cap) {
        m->cap *= 2;
        m->keys = jig_realloc(m->keys, (size_t)m->cap * k->size);
        m->vals = jig_realloc(m->vals, (size_t)m->cap * v->size);
    }
    memcpy(m->keys + (size_t)m->len * k->size, key, k->size);
    memcpy(m->vals + (size_t)m->len * v->size, val, v->size);
    m->len++;
    if (m->len * 2 + 2 > m->index_cap) {
        map_rebuild_index(m, k);
    } else {
        uint64_t h = k->hash(key);
        int64_t mask = m->index_cap - 1;
        int64_t slot = (int64_t)(h & (uint64_t)mask);
        while (m->index[slot] >= 0) slot = (slot + 1) & mask;
        m->index[slot] = (int32_t)(m->len - 1);
    }
}

bool jig_map_get(const jig_map *m, const void *key, void *out, const jig_type *k, const jig_type *v) {
    int64_t idx = map_find(m, key, k);
    if (idx < 0) return false;
    memcpy(out, m->vals + (size_t)idx * v->size, v->size);
    if (v->retain) v->retain(out);
    return true;
}

bool jig_map_contains(const jig_map *m, const void *key, const jig_type *k) { return map_find(m, key, k) >= 0; }

bool jig_map_remove(jig_map **mp, const void *key, void *out, const jig_type *k, const jig_type *v) {
    if (map_find(*mp, key, k) < 0) return false;
    map_make_unique(mp, k, v);
    jig_map *m = *mp;
    int64_t idx = map_find(m, key, k);
    if (k->release) k->release(m->keys + (size_t)idx * k->size);
    if (out) {
        memcpy(out, m->vals + (size_t)idx * v->size, v->size);
    } else if (v->release) {
        v->release(m->vals + (size_t)idx * v->size);
    }
    int64_t rest = m->len - idx - 1;
    memmove(m->keys + (size_t)idx * k->size, m->keys + (size_t)(idx + 1) * k->size, (size_t)rest * k->size);
    memmove(m->vals + (size_t)idx * v->size, m->vals + (size_t)(idx + 1) * v->size, (size_t)rest * v->size);
    m->len--;
    map_rebuild_index(m, k);
    return true;
}

static jig_list *copy_array(const char *data, int64_t n, const jig_type *t) {
    if (!n) return NULL;
    jig_list *l = jig_list_with_capacity(n, t);
    memcpy(l->data, data, (size_t)n * t->size);
    l->len = n;
    if (t->retain)
        for (int64_t i = 0; i < n; i++) t->retain(l->data + (size_t)i * t->size);
    return l;
}

jig_list *jig_map_keys(const jig_map *m, const jig_type *k) { return m ? copy_array(m->keys, m->len, k) : NULL; }
jig_list *jig_map_values(const jig_map *m, const jig_type *v) { return m ? copy_array(m->vals, m->len, v) : NULL; }
void *jig_map_key_at(const jig_map *m, int64_t i, const jig_type *k) { return m->keys + (size_t)i * k->size; }
void *jig_map_val_at(const jig_map *m, int64_t i, const jig_type *v) { return m->vals + (size_t)i * v->size; }

bool jig_map_eq(const jig_map *a, const jig_map *b, const jig_type *k, const jig_type *v) {
    if (jig_map_len(a) != jig_map_len(b)) return false;
    for (int64_t i = 0; i < jig_map_len(a); i++) {
        int64_t j = map_find(b, a->keys + (size_t)i * k->size, k);
        if (j < 0 || !v->eq(a->vals + (size_t)i * v->size, b->vals + (size_t)j * v->size)) return false;
    }
    return true;
}

void jig_map_show(jig_buf *b, const jig_map *m, const jig_type *k, const jig_type *v, bool is_set) {
    jig_buf_push(b, "{", 1);
    for (int64_t i = 0; i < jig_map_len(m); i++) {
        if (i) jig_buf_push(b, ", ", 2);
        k->show(b, m->keys + (size_t)i * k->size, true);
        if (!is_set) {
            jig_buf_push(b, ": ", 2);
            v->show(b, m->vals + (size_t)i * v->size, true);
        }
    }
    jig_buf_push(b, "}", 1);
}

/* ---------- checked arithmetic ---------- */

void jig_overflow(const char *op, const char *loc) {
    char msg[256];
    snprintf(msg, sizeof msg, "arithmetic overflow: %s", op);
    jig_panic(msg, loc);
}

void jig_div_zero(const char *loc) { jig_panic("division by zero", loc); }

void jig_shift_range(int64_t amount, const char *loc) {
    char msg[128];
    snprintf(msg, sizeof msg, "shift amount %lld is out of range", (long long)amount);
    jig_panic(msg, loc);
}

void jig_cast_range_i(int64_t v, const char *ty, const char *loc) {
    char msg[128];
    snprintf(msg, sizeof msg, "cannot convert %lld to `%s`: out of range", (long long)v, ty);
    jig_panic(msg, loc);
}

void jig_cast_range_u(uint64_t v, const char *ty, const char *loc) {
    char msg[128];
    snprintf(msg, sizeof msg, "cannot convert %llu to `%s`: out of range", (unsigned long long)v, ty);
    jig_panic(msg, loc);
}

void jig_cast_range_f(double v, const char *ty, const char *loc) {
    jig_buf b = {0};
    jig_buf_push_cstr(&b, "cannot convert ");
    jig_show_f64(&b, v);
    jig_buf_push_cstr(&b, " to `");
    jig_buf_push_cstr(&b, ty);
    jig_buf_push_cstr(&b, "`: out of range");
    jig_panic_buf(&b, loc);
}

void jig_index_oob(int64_t i, int64_t len, const char *loc) {
    char msg[128];
    snprintf(msg, sizeof msg, "index out of bounds: the index is %lld but the length is %lld", (long long)i, (long long)len);
    jig_panic(msg, loc);
}

int64_t jig_ipow(int64_t base, int64_t exp, const char *loc) {
    if (exp < 0) jig_panic("negative exponent", loc);
    int64_t r = 1;
    for (int64_t i = 0; i < exp; i++)
        if (__builtin_mul_overflow(r, base, &r)) jig_overflow("pow", loc);
    return r;
}

/* ---------- formatting ---------- */

/* Decimal digits of `v`, written backwards so they end at `end`; returns their count. */
static int jig_digits(char *end, uint64_t v) {
    static const char pairs[201] =
        "00010203040506070809101112131415161718192021222324252627282930313233343536373839"
        "40414243444546474849505152535455565758596061626364656667686970717273747576777879"
        "8081828384858687888990919293949596979899";
    char *p = end;
    while (v >= 100) {
        unsigned r = (unsigned)(v % 100);
        v /= 100;
        p -= 2;
        memcpy(p, pairs + 2 * r, 2);
    }
    if (v >= 10) {
        p -= 2;
        memcpy(p, pairs + 2 * v, 2);
    } else {
        *--p = (char)('0' + v);
    }
    return (int)(end - p);
}

/* `v` in decimal, ending at `end` (at least 20 bytes after the start); returns its length. */
static int jig_fmt_i64(char *end, int64_t v) {
    uint64_t u = v < 0 ? (uint64_t)0 - (uint64_t)v : (uint64_t)v;
    int n = jig_digits(end, u);
    if (v < 0) end[-++n] = '-';
    return n;
}

void jig_show_i64(jig_buf *b, int64_t v) {
    char tmp[24];
    int n = jig_fmt_i64(tmp + sizeof tmp, v);
    jig_buf_push(b, tmp + sizeof tmp - n, n);
}

void jig_show_u64(jig_buf *b, uint64_t v) {
    char tmp[24];
    int n = jig_digits(tmp + sizeof tmp, v);
    jig_buf_push(b, tmp + sizeof tmp - n, n);
}

jig_str *jig_i64_str(int64_t v) {
    char tmp[24];
    int n = jig_fmt_i64(tmp + sizeof tmp, v);
    return jig_str_new(tmp + sizeof tmp - n, n);
}

jig_str *jig_u64_str(uint64_t v) {
    char tmp[24];
    int n = jig_digits(tmp + sizeof tmp, v);
    return jig_str_new(tmp + sizeof tmp - n, n);
}

/* Shortest round-trip formatting, matching Rust's `{:?}` for f64. */
void jig_show_f64(jig_buf *b, double v) {
    if (isnan(v)) {
        jig_buf_push_cstr(b, "NaN");
        return;
    }
    if (isinf(v)) {
        jig_buf_push_cstr(b, v > 0 ? "inf" : "-inf");
        return;
    }
    char tmp[64];
    int p;
    for (p = 1; p <= 17; p++) {
        snprintf(tmp, sizeof tmp, "%.*e", p - 1, v);
        if (strtod(tmp, NULL) == v) break;
    }
    /* tmp: [-]d[.ddd]e[+-]XX */
    const char *s = tmp;
    bool neg = *s == '-';
    if (neg) s++;
    char digits[32];
    int nd = 0;
    while (*s && *s != 'e') {
        if (*s != '.') digits[nd++] = *s;
        s++;
    }
    int exp = atoi(s + 1);
    while (nd > 1 && digits[nd - 1] == '0') nd--;
    double a = fabs(v);
    if (neg) jig_buf_push(b, "-", 1);
    if (a == 0 || (a >= 1e-4 && a < 1e16)) {
        if (exp >= 0) {
            for (int i = 0; i <= exp; i++) jig_buf_push(b, i < nd ? &digits[i] : "0", 1);
            jig_buf_push(b, ".", 1);
            if (exp + 1 < nd)
                jig_buf_push(b, digits + exp + 1, nd - exp - 1);
            else
                jig_buf_push(b, "0", 1);
        } else {
            jig_buf_push(b, "0.", 2);
            for (int i = 0; i < -exp - 1; i++) jig_buf_push(b, "0", 1);
            jig_buf_push(b, digits, nd);
        }
    } else {
        jig_buf_push(b, digits, 1);
        if (nd > 1) {
            jig_buf_push(b, ".", 1);
            jig_buf_push(b, digits + 1, nd - 1);
        }
        char e[16];
        int n = snprintf(e, sizeof e, "e%d", exp);
        jig_buf_push(b, e, n);
    }
}

void jig_show_bool(jig_buf *b, bool v) { jig_buf_push_cstr(b, v ? "true" : "false"); }

static void escape_char(jig_buf *b, uint32_t c, char quote) {
    switch (c) {
    case '\t': jig_buf_push(b, "\\t", 2); return;
    case '\r': jig_buf_push(b, "\\r", 2); return;
    case '\n': jig_buf_push(b, "\\n", 2); return;
    case '\\': jig_buf_push(b, "\\\\", 2); return;
    case 0: jig_buf_push(b, "\\0", 2); return;
    default: break;
    }
    if (c == (uint32_t)quote) {
        jig_buf_push(b, "\\", 1);
        jig_buf_push(b, &quote, 1);
        return;
    }
    if (c < 0x20 || c == 0x7f) {
        char tmp[16];
        int n = snprintf(tmp, sizeof tmp, "\\u{%x}", c);
        jig_buf_push(b, tmp, n);
        return;
    }
    encode_utf8(b, c);
}

void jig_show_char(jig_buf *b, uint32_t c, bool nested) {
    if (!nested) {
        encode_utf8(b, c);
        return;
    }
    jig_buf_push(b, "'", 1);
    escape_char(b, c, '\'');
    jig_buf_push(b, "'", 1);
}

void jig_show_str(jig_buf *b, const jig_str *s, bool nested) {
    if (!nested) {
        jig_buf_push_str(b, s);
        return;
    }
    jig_buf_push(b, "\"", 1);
    const char *d = jig_str_data(s);
    int64_t n = jig_str_len(s);
    for (int64_t i = 0; i < n;) escape_char(b, decode_utf8(d, &i, n), '"');
    jig_buf_push(b, "\"", 1);
}

/* ---------- characters (ASCII fast paths; others via towupper-like ranges) ---------- */

bool jig_char_is_digit(uint32_t c) { return c >= '0' && c <= '9'; }
bool jig_char_is_upper(uint32_t c) { return (c >= 'A' && c <= 'Z') || (c >= 0xC0 && c <= 0xDE && c != 0xD7); }
bool jig_char_is_lower(uint32_t c) { return (c >= 'a' && c <= 'z') || (c >= 0xDF && c <= 0xFF && c != 0xF7); }
bool jig_char_is_alpha(uint32_t c) { return jig_char_is_upper(c) || jig_char_is_lower(c) || (c > 0xFF && c != 0x3000); }
bool jig_char_is_alnum(uint32_t c) { return jig_char_is_alpha(c) || jig_char_is_digit(c); }
bool jig_char_is_space(uint32_t c) { return c == ' ' || (c >= 9 && c <= 13) || c == 0x85 || c == 0xA0 || c == 0x3000; }
uint32_t jig_char_to_upper(uint32_t c) {
    if ((c >= 'a' && c <= 'z') || (c >= 0xE0 && c <= 0xFE && c != 0xF7)) return c - 32;
    return c;
}
uint32_t jig_char_to_lower(uint32_t c) {
    if ((c >= 'A' && c <= 'Z') || (c >= 0xC0 && c <= 0xDE && c != 0xD7)) return c + 32;
    return c;
}
uint32_t jig_char_from_int(int64_t v, const char *loc) {
    if (v < 0 || v > 0x10FFFF || (v >= 0xD800 && v <= 0xDFFF)) {
        char msg[96];
        snprintf(msg, sizeof msg, "%lld is not a valid `Char`", (long long)v);
        jig_panic(msg, loc);
    }
    return (uint32_t)v;
}

/* ---------- effects ---------- */

void jig_print(const jig_str *s, bool newline, bool err) {
    if (err) {
        flush_out();
        fwrite(jig_str_data(s), 1, (size_t)jig_str_len(s), stderr);
        if (newline) fputc('\n', stderr);
        return;
    }
    const char *d = jig_str_data(s);
    size_t n = (size_t)jig_str_len(s);
    if (g_out_len + n + 1 > sizeof g_out) flush_out();
    if (n + 1 > sizeof g_out) {
        fwrite(d, 1, n, stdout);
    } else {
        memcpy(g_out + g_out_len, d, n);
        g_out_len += n;
    }
    if (newline) g_out[g_out_len++] = '\n';
}

jig_str *jig_read_line(bool *ok) {
    flush_out();
    char *line = NULL;
    size_t cap = 0;
    ssize_t n = getline(&line, &cap, stdin);
    if (n < 0) {
        free(line);
        *ok = false;
        return NULL;
    }
    while (n > 0 && (line[n - 1] == '\n' || line[n - 1] == '\r')) n--;
    jig_str *s = jig_str_new(line, n);
    free(line);
    *ok = true;
    return s;
}

jig_str *jig_read_all(void) {
    flush_out();
    jig_buf b = {0};
    char tmp[1 << 14];
    size_t n;
    while ((n = fread(tmp, 1, sizeof tmp, stdin)) > 0) jig_buf_push(&b, tmp, (int64_t)n);
    return jig_buf_to_str(&b);
}

static char *cpath(const jig_str *p) {
    char *s = jig_alloc((size_t)jig_str_len(p) + 1);
    memcpy(s, jig_str_data(p), (size_t)jig_str_len(p));
    s[jig_str_len(p)] = 0;
    return s;
}

static jig_str *err_str(const jig_str *path) {
    jig_buf b = {0};
    jig_buf_push_str(&b, path);
    jig_buf_push_cstr(&b, ": ");
    jig_buf_push_cstr(&b, strerror(errno));
    return jig_buf_to_str(&b);
}

jig_str *jig_fs_read(const jig_str *path, bool *ok) {
    char *p = cpath(path);
    FILE *f = fopen(p, "rb");
    jig_free(p);
    if (!f) {
        *ok = false;
        return err_str(path);
    }
    jig_buf b = {0};
    char tmp[1 << 14];
    size_t n;
    while ((n = fread(tmp, 1, sizeof tmp, f)) > 0) jig_buf_push(&b, tmp, (int64_t)n);
    fclose(f);
    *ok = true;
    return jig_buf_to_str(&b);
}

jig_str *jig_fs_write(const jig_str *path, const jig_str *contents, bool *ok) {
    char *p = cpath(path);
    FILE *f = fopen(p, "wb");
    jig_free(p);
    if (!f) {
        *ok = false;
        return err_str(path);
    }
    fwrite(jig_str_data(contents), 1, (size_t)jig_str_len(contents), f);
    fclose(f);
    *ok = true;
    return NULL;
}

bool jig_fs_exists(const jig_str *path) {
    char *p = cpath(path);
    struct stat st;
    bool r = stat(p, &st) == 0;
    jig_free(p);
    return r;
}

static int cmp_strp(const void *a, const void *b) { return jig_str_cmp(*(jig_str *const *)a, *(jig_str *const *)b); }

jig_list *jig_fs_list(const jig_str *dir, bool *ok, jig_str **err) {
    char *p = cpath(dir);
    DIR *d = opendir(p);
    jig_free(p);
    if (!d) {
        *ok = false;
        *err = err_str(dir);
        return NULL;
    }
    jig_list *out = NULL;
    struct dirent *e;
    while ((e = readdir(d))) {
        if (!strcmp(e->d_name, ".") || !strcmp(e->d_name, "..")) continue;
        push_str_elem(&out, e->d_name, (int64_t)strlen(e->d_name));
    }
    closedir(d);
    if (out) qsort(out->data, (size_t)out->len, sizeof(jig_str *), cmp_strp);
    *ok = true;
    return out;
}

jig_list *jig_args(void) {
    jig_list *out = NULL;
    for (int i = 0; i < g_argc; i++) push_str_elem(&out, g_argv[i], (int64_t)strlen(g_argv[i]));
    return out;
}

jig_str *jig_env_var(const jig_str *name, bool *ok) {
    char *p = cpath(name);
    const char *v = getenv(p);
    jig_free(p);
    *ok = v != NULL;
    return v ? jig_str_new(v, (int64_t)strlen(v)) : NULL;
}

int64_t jig_now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    return (int64_t)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

void jig_sleep_ms(int64_t ms, const char *loc) {
    if (ms < 0) jig_panic("precondition failed: ms >= 0", loc);
    struct timespec ts = {ms / 1000, (ms % 1000) * 1000000};
    while (nanosleep(&ts, &ts) != 0 && errno == EINTR) {
    }
}

/* ---------- net ---------- */

static jig_str *net_err(const jig_str *addr, const char *what) {
    jig_buf b = {0};
    jig_buf_push_str(&b, addr);
    jig_buf_push_cstr(&b, ": ");
    jig_buf_push_cstr(&b, what);
    return jig_buf_to_str(&b);
}

/* "host:port" (":port" is this machine) resolved for connecting or listening. */
static struct addrinfo *net_resolve(const jig_str *addr, bool listen, const char **err) {
    int64_t n = jig_str_len(addr), colon = -1;
    const char *d = jig_str_data(addr);
    for (int64_t i = 0; i < n; i++)
        if (d[i] == ':') colon = i;
    if (colon < 0 || colon == n - 1) {
        *err = "expected host:port";
        return NULL;
    }
    char host[256], port[32];
    if (colon >= (int64_t)sizeof host || n - colon - 1 >= (int64_t)sizeof port) {
        *err = "address too long";
        return NULL;
    }
    memcpy(host, d, (size_t)colon);
    host[colon] = 0;
    memcpy(port, d + colon + 1, (size_t)(n - colon - 1));
    port[n - colon - 1] = 0;
    struct addrinfo hints = {0}, *res = NULL;
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    if (listen) hints.ai_flags = AI_PASSIVE;
    const char *h = colon ? host : (listen ? NULL : "127.0.0.1");
    int rc = getaddrinfo(h, port, &hints, &res);
    if (rc != 0) {
        *err = gai_strerror(rc);
        return NULL;
    }
    return res;
}

static bool net_send_all(int fd, const char *p, size_t n) {
    while (n) {
        ssize_t k = send(fd, p, n, MSG_NOSIGNAL);
        if (k < 0 && errno == EINTR) continue;
        if (k <= 0) return false;
        p += k;
        n -= (size_t)k;
    }
    return true;
}

static jig_str *net_read_all(int fd) {
    jig_buf b = {0};
    char tmp[1 << 14];
    for (;;) {
        ssize_t k = recv(fd, tmp, sizeof tmp, 0);
        if (k < 0 && errno == EINTR) continue;
        if (k <= 0) break;
        jig_buf_push(&b, tmp, (int64_t)k);
    }
    return jig_buf_to_str(&b);
}

jig_str *jig_net_request(const jig_str *addr, const jig_str *req, bool *ok) {
    const char *err = NULL;
    struct addrinfo *res = net_resolve(addr, false, &err);
    *ok = false;
    if (!res) return net_err(addr, err);
    int fd = -1;
    for (struct addrinfo *ai = res; ai; ai = ai->ai_next) {
        fd = socket(ai->ai_family, ai->ai_socktype, ai->ai_protocol);
        if (fd < 0) continue;
        if (connect(fd, ai->ai_addr, ai->ai_addrlen) == 0) break;
        close(fd);
        fd = -1;
    }
    freeaddrinfo(res);
    if (fd < 0) return net_err(addr, strerror(errno));
    if (!net_send_all(fd, jig_str_data(req), (size_t)jig_str_len(req))) {
        close(fd);
        return net_err(addr, strerror(errno));
    }
    shutdown(fd, SHUT_WR);
    jig_str *reply = net_read_all(fd);
    close(fd);
    *ok = true;
    return reply;
}

jig_str *jig_net_serve(const jig_str *addr, void *state, jig_fn *f, jig_serve_call call, int64_t limit, bool *ok,
                       const char *loc) {
    if (limit < 0) jig_panic("precondition failed: limit >= 0", loc);
    const char *err = NULL;
    struct addrinfo *res = net_resolve(addr, true, &err);
    *ok = false;
    if (!res) return net_err(addr, err);
    int fd = socket(res->ai_family, res->ai_socktype, res->ai_protocol);
    int one = 1;
    if (fd >= 0) setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
    if (fd < 0 || bind(fd, res->ai_addr, res->ai_addrlen) != 0 || listen(fd, 64) != 0) {
        jig_str *e = net_err(addr, strerror(errno));
        if (fd >= 0) close(fd);
        freeaddrinfo(res);
        return e;
    }
    freeaddrinfo(res);
    for (int64_t served = 0; limit == 0 || served < limit; served++) {
        int c = accept(fd, NULL, NULL);
        if (c < 0) {
            if (errno == EINTR) {
                served--;
                continue;
            }
            jig_str *e = net_err(addr, strerror(errno));
            close(fd);
            return e;
        }
        jig_str *req = net_read_all(c);
        jig_str *reply = call(f, state, req);
        net_send_all(c, jig_str_data(reply), (size_t)jig_str_len(reply));
        close(c);
        jig_str_release(req);
        jig_str_release(reply);
    }
    close(fd);
    *ok = true;
    return NULL;
}

int64_t jig_now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec;
}

static uint64_t next_rand(void) {
    uint64_t x = g_rng;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    g_rng = x;
    return x * 0x2545f4914f6cdd1dull;
}

int64_t jig_rand_int(int64_t lo, int64_t hi, const char *loc) {
    if (lo >= hi) jig_panic("precondition failed: lo < hi", loc);
    uint64_t span = (uint64_t)hi - (uint64_t)lo;
    return (int64_t)((uint64_t)lo + next_rand() % span);
}

double jig_rand_float(void) { return (double)(next_rand() >> 11) / (double)(1ull << 53); }

/* ---------- runtime element types ---------- */

static void str_retain(void *p) { jig_str_retain(*(jig_str **)p); }
static void str_release(void *p) { jig_str_release(*(jig_str **)p); }
static bool str_eq(const void *a, const void *b) { return jig_str_eq(*(jig_str *const *)a, *(jig_str *const *)b); }
static int str_cmp(const void *a, const void *b) { return jig_str_cmp(*(jig_str *const *)a, *(jig_str *const *)b); }
static uint64_t str_hash(const void *a) { return jig_str_hash(*(jig_str *const *)a); }
static void str_show(jig_buf *b, const void *a, bool nested) { jig_show_str(b, *(jig_str *const *)a, nested); }
const jig_type jig_type_str = {sizeof(jig_str *), str_retain, str_release, str_eq, str_cmp, str_hash, str_show};

static bool u8_eq(const void *a, const void *b) { return *(const uint8_t *)a == *(const uint8_t *)b; }
static int u8_cmp(const void *a, const void *b) { return (int)*(const uint8_t *)a - (int)*(const uint8_t *)b; }
static uint64_t u8_hash(const void *a) { return jig_hash_u64(*(const uint8_t *)a); }
static void u8_show(jig_buf *b, const void *a, bool nested) {
    (void)nested;
    jig_show_i64(b, *(const uint8_t *)a);
}
const jig_type jig_type_u8 = {1, NULL, NULL, u8_eq, u8_cmp, u8_hash, u8_show};

static bool char_eq(const void *a, const void *b) { return *(const uint32_t *)a == *(const uint32_t *)b; }
static int char_cmp(const void *a, const void *b) {
    uint32_t x = *(const uint32_t *)a, y = *(const uint32_t *)b;
    return x < y ? -1 : x > y;
}
static uint64_t char_hash(const void *a) { return jig_hash_u64(*(const uint32_t *)a); }
static void char_show(jig_buf *b, const void *a, bool nested) { jig_show_char(b, *(const uint32_t *)a, nested); }
const jig_type jig_type_char = {4, NULL, NULL, char_eq, char_cmp, char_hash, char_show};
