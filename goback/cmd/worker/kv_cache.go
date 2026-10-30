//go:build js && wasm

package main

import (
	"strings"
	"time"

	"github.com/syumai/workers-go/cloudflare/kv"
)

// kvCache is the write-through response cache backed by the KV binding
// "CACHE". Keys (v4/notes/{id}.json, v4/index.json) and payloads match the
// Rust worker, so entries can be shared while switching workers.
type kvCache struct {
	ns *kv.Namespace
}

func newKVCache(ns *kv.Namespace) *kvCache { return &kvCache{ns: ns} }

func (c *kvCache) Get(key string) ([]byte, bool) {
	if c.ns == nil {
		return nil, false
	}
	// KV resolves a miss to null, which GetString renders as "<null>".
	// Real payloads are JSON objects, so require a leading '{'.
	s, err := c.ns.GetString(key, nil)
	if err != nil || !strings.HasPrefix(s, "{") {
		return nil, false
	}
	return []byte(s), true
}

func (c *kvCache) Put(key string, payload []byte, ttl time.Duration) {
	if c.ns == nil {
		return
	}
	secs := int(ttl.Seconds())
	if secs < kvMinTTL {
		secs = kvMinTTL
	}
	// On failure delete the key so the next read falls back to D1 instead of
	// serving stale data.
	if err := c.ns.PutString(key, string(payload), &kv.PutOptions{ExpirationTTL: secs}); err != nil {
		_ = c.ns.Delete(key)
	}
}

func (c *kvCache) Delete(key string) {
	if c.ns == nil {
		return
	}
	_ = c.ns.Delete(key)
}

func (c *kvCache) NoteTTL() time.Duration  { return noteTTL }
func (c *kvCache) IndexTTL() time.Duration { return indexTTL }
