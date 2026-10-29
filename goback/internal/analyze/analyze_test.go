package analyze

import (
	"strings"
	"testing"

	"urlnote/goback/internal/token"
	"urlnote/goback/shared"
)

func hasIssue(issues []shared.Issue, contains string) bool {
	for _, i := range issues {
		if strings.Contains(i.Problem+i.Suggestion+i.Quote, contains) {
			return true
		}
	}
	return false
}

func TestAnalyzeSmoke(t *testing.T) {
	a := token.New()
	defer a.Close()
	cases := []struct {
		text string
		want []string // substrings expected among the issues
	}{
		{"我的的便签,,测试", []string{"连续出现", "标点符号重复"}},
		{"他跑的很快。慢慢的跑", nil}, // 跑得很快 without 得 is not flagged by windows(3) on short text; just ensure no panic
		{"すもももももももものうちをを食べる", []string{"助詞"}},
		{"（未完成的文章", []string{"多 1 处"}},
		{"很好很好", []string{"重复"}},
	}
	for _, c := range cases {
		issues := Analyze(a, c.text)
		for _, want := range c.want {
			if !hasIssue(issues, want) {
				t.Fatalf("%q: missing issue containing %q; got %+v", c.text, want, issues)
			}
		}
	}
}

func TestAnalyzeEmpty(t *testing.T) {
	a := token.New()
	defer a.Close()
	if got := Analyze(a, "  \n\t "); len(got) != 0 {
		t.Fatalf("expected no issues for blank text, got %+v", got)
	}
}
