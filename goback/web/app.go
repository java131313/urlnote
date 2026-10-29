//go:build js && wasm

package main

import (
	"strings"

	"urlnote/goback/shared"
)

// API_BASE / BASE_PATH mirror the Rust compile-time deployment config. Set
// them via -ldflags "-X main.API_BASE=..." for split deployments (frontend
// on GitHub Pages, API elsewhere); empty keeps same-origin behavior.
var (
	API_BASE  string
	BASE_PATH string
)

func apiURL(path string) string {
	if API_BASE == "" {
		return path
	}
	return API_BASE + path
}

// display_md rewrites stored relative /i/ links to absolute for cross-origin
// display; storage_md is its inverse, applied before persisting.
func displayMD(md string) string {
	if API_BASE == "" {
		return md
	}
	return strings.ReplaceAll(md, "](/i/", "]("+API_BASE+"/i/")
}

func storageMD(md string) string {
	if API_BASE == "" {
		return md
	}
	return strings.ReplaceAll(md, "]("+API_BASE+"/i/", "](/i/")
}

// App holds all application state: the Leptos Ctx signals and AppState
// fields collapse into this one struct, mutated by handlers with explicit
// re-render calls.
type App struct {
	id   string
	user string

	editor  jsEditor
	hasEdit bool

	// timers
	pollTimer, presenceTimer, lockTimer, autosaveTimer jsValue

	// editor state
	dirty              bool
	loaded             bool
	applyingRemote     bool
	pollUpdatedAt      string
	suppressInputUntil float64
	lastApplied       string
	baselineFont      int
	baselineColor     string
	baselineTitle     string
	baselineTags      string

	// header / settings
	fontSize int
	color    string
	title    string
	tagsText string
	author   string

	// status
	statusText  string
	statusClass string

	remoteChanged  bool
	presenceOthers []string
	showLogin      bool
	markMode       bool

	// picker
	pickerOpen  bool
	pickerQuery string
	allNotes    []shared.NoteMeta
	searchTimer jsValue

	// edit lock
	isAdmin        bool
	editLocked     bool
	canEdit        bool
	editRequest    string
	editRequests   []shared.EditRequestItem
	reqPanelOpen   bool
	reqMessage     string
	lockDismissed  bool
	lockApplyOpen  bool

	// suggestions
	suggestAnchor       jsValue
	suggestAnchorStart  int
	suggestAnchorEnd    int
	suggestAcceptedWord string
	suggestItems        []suggestItem
	suggestActive       int
	suggestPos          [2]int

	// pos overlay
	posOn            bool
	posSeq           int
	posTokens        []shared.PosToken
	posTaggedChars   int
	markExists       bool
	posScrollDone    bool
	posDebounce      jsValue
	posZoomTimer     jsValue

	// find
	findOpen     bool
	findQuery    string
	findCount    int
	findAll      int
	findRanges   [][2]int
	findDebounce jsValue

	// analyze
	analyzeOpen    bool
	analyzeBusy    bool
	analyzeIssues  []shared.Issue
	analyzeApplied map[int]bool

	// DOM element refs (populated by renderMain)
	editorDiv      jsValue
	titleInput     jsValue
	tagsInput      jsValue
	tagsChips      jsValue
	authorBadge    jsValue
	presenceBadge  jsValue
	pickerPanel    jsValue
	pickerList     jsValue
	pickerBtn      jsValue
	lockBtn        jsValue
	reqSpan        jsValue
	reqBtn         jsValue
	reqListPanel   jsValue
	reqList        jsValue
	posBtn         jsValue
	findBtn        jsValue
	findInput      jsValue
	findMeta       jsValue
	findMetaAll    jsValue
	uploadBtn      jsValue
	appendBtn      jsValue
	fontReset      jsValue
	colorInput     jsValue
	colorReset     jsValue
	markBanner     jsValue
	syncBanner     jsValue
	posLegend      jsValue
	posOverlay     jsValue
}

type suggestItem struct {
	word string
	isJA bool
}

var app *App

func newApp(id string) *App {
	return &App{
		id:               id,
		fontSize:         shared.DefaultFontSize,
		canEdit:          true, // default unlocked until first /api/edit-status
		suggestAnchor:    jsUndefined(),
		analyzeApplied:   map[int]bool{},
		suggestItems:     []suggestItem{},
		presenceOthers:   []string{},
		allNotes:         []shared.NoteMeta{},
		editRequests:     []shared.EditRequestItem{},
		posTokens:        []shared.PosToken{},
		findRanges:       [][2]int{},
		analyzeIssues:    []shared.Issue{},
		posTaggedChars:   0,
		suggestPos:       [2]int{40, 40},
	}
}

// parsedTags mirrors the Rust tags memo: split on ,/， trim, drop empties.
func (a *App) parsedTags() []string {
	out := []string{}
	for _, t := range strings.FieldsFunc(a.tagsText, func(r rune) bool { return r == ',' || r == '，' }) {
		if t = strings.TrimSpace(t); t != "" {
			out = append(out, t)
		}
	}
	return out
}

// ---- status ----

func (a *App) setStatus(text, cls string) {
	a.statusText, a.statusClass = text, cls
	e := qs("#status")
	if e.Truthy() {
		e.Set("textContent", text)
		e.Set("className", cls)
	}
}

// pendingCount counts pending edit requests (for the admin button label).
func (a *App) pendingCount() int {
	n := 0
	for _, r := range a.editRequests {
		if r.Status == "pending" {
			n++
		}
	}
	return n
}
