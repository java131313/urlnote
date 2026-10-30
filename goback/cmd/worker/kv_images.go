//go:build js && wasm

package main

import (
	"bytes"
	"errors"
	"io"

	"github.com/syumai/workers-go/cloudflare/kv"
)

const imageKeyPrefix = "img/"

// kvImages stores uploaded image binaries in the KV binding "CACHE" under
// "img/<random name>" — random keys, immutable content, same layout as the
// Rust worker.
type kvImages struct {
	ns *kv.Namespace
}

func newKVImages(ns *kv.Namespace) *kvImages { return &kvImages{ns: ns} }

func (k *kvImages) Put(name string, data []byte) error {
	if k.ns == nil {
		return errors.New("kv binding CACHE unavailable")
	}
	// PutReader (not PutString): the string API round-trips through UTF-8 and
	// would corrupt binary image data.
	return k.ns.PutReader(imageKeyPrefix+name, bytes.NewReader(data), nil)
}

func (k *kvImages) Get(name string) ([]byte, bool, error) {
	if k.ns == nil {
		return nil, false, nil
	}
	key := imageKeyPrefix + name
	// Probe existence first: GetReader on a miss returns a null stream whose
	// first Read panics.
	list, err := k.ns.List(&kv.ListOptions{Prefix: key, Limit: 1})
	if err != nil || len(list.Keys) == 0 || list.Keys[0].Name != key {
		return nil, false, nil
	}
	r, err := k.ns.GetReader(key, nil)
	if err != nil {
		return nil, false, nil
	}
	b, err := io.ReadAll(r)
	if err != nil {
		return nil, false, err
	}
	return b, true, nil
}
