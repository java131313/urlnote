// Package cache is a small in-memory TTL cache for note payloads and the
// note index, mirroring the Rust worker's per-isolate LRU. On a single
// self-hosted instance a short TTL keeps reads cheap while bounding how long
// a write handled elsewhere stays invisible (the frontend polls updated_at,
// so worst-case sync lag is one TTL).
package cache

import (
	"sync"
	"time"
)

type entry struct {
	payload []byte
	expires time.Time
}

// Cache is a mutex-guarded string-keyed TTL cache with naive cap eviction
// (clearing the oldest entries when full).
type Cache struct {
	mu      sync.RWMutex
	items   map[string]entry
	order   []string // insertion order, for eviction
	cap     int
	ttlNote time.Duration
	ttlIdx  time.Duration
}

func New(capacity int, ttlNote, ttlIndex time.Duration) *Cache {
	return &Cache{items: make(map[string]entry, capacity), cap: capacity, ttlNote: ttlNote, ttlIdx: ttlIndex}
}

func (c *Cache) Get(key string) ([]byte, bool) {
	c.mu.RLock()
	e, ok := c.items[key]
	c.mu.RUnlock()
	if !ok {
		return nil, false
	}
	if time.Now().After(e.expires) {
		c.Delete(key)
		return nil, false
	}
	return e.payload, true
}

func (c *Cache) Put(key string, payload []byte, ttl time.Duration) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if _, exists := c.items[key]; !exists {
		c.order = append(c.order, key)
	}
	c.items[key] = entry{payload: payload, expires: time.Now().Add(ttl)}
	for len(c.order) >= c.cap {
		oldest := c.order[0]
		c.order = c.order[1:]
		delete(c.items, oldest)
	}
}

func (c *Cache) Delete(key string) {
	c.mu.Lock()
	defer c.mu.Unlock()
	delete(c.items, key)
	for i, k := range c.order {
		if k == key {
			c.order = append(c.order[:i], c.order[i+1:]...)
			break
		}
	}
}

// NoteTTL / IndexTTL expose the configured TTLs so callers don't hardcode.
func (c *Cache) NoteTTL() time.Duration  { return c.ttlNote }
func (c *Cache) IndexTTL() time.Duration { return c.ttlIdx }
