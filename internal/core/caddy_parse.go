package core

import (
	"slices"
	"strings"
)

type caddyToken struct {
	value             string
	start, end, depth int
	quoted            bool
}

func caddyTokens(source string) ([]caddyToken, error) {
	out := []caddyToken{}
	depth := 0
	for pos := 0; pos < len(source); {
		if strings.ContainsRune(" \t\r\n", rune(source[pos])) {
			pos++
			continue
		}
		if source[pos] == '#' {
			for pos < len(source) && source[pos] != '\n' {
				pos++
			}
			continue
		}
		start := pos
		quoted := source[pos] == '"' || source[pos] == '`'
		if strings.HasPrefix(source[pos:], "<<") {
			lineEnd := strings.IndexByte(source[pos:], '\n')
			if lineEnd < 0 {
				return nil, invalid("Caddy heredoc 缺少正文")
			}
			lineEnd += pos
			marker := strings.TrimSuffix(source[pos+2:lineEnd], "\r")
			if !siteIdentifier.MatchString(marker) {
				return nil, invalid("Caddy heredoc 标记无效")
			}
			pos = lineEnd + 1
			closed := false
			for pos < len(source) {
				end := strings.IndexByte(source[pos:], '\n')
				if end < 0 {
					end = len(source)
				} else {
					end += pos
				}
				if strings.TrimSpace(source[pos:end]) == marker {
					pos = end
					closed = true
					break
				}
				pos = end
				if pos < len(source) {
					pos++
				}
			}
			if !closed {
				return nil, invalid("Caddy heredoc 未闭合")
			}
		} else if quoted {
			quote := source[pos]
			pos++
			closed := false
			for pos < len(source) {
				if source[pos] == '\\' && quote == '"' {
					pos += 2
					continue
				}
				if source[pos] == quote {
					pos++
					closed = true
					break
				}
				pos++
			}
			if !closed || pos > len(source) {
				return nil, invalid("Caddy 引号未闭合")
			}
		} else {
			for pos < len(source) && !strings.ContainsRune(" \t\r\n", rune(source[pos])) {
				pos++
			}
		}
		value := source[start:pos]
		if !quoted && value == "}" {
			depth--
			if depth < 0 {
				return nil, invalid("Caddy 配置块边界无效")
			}
		}
		out = append(out, caddyToken{value, start, pos, depth, quoted})
		if !quoted && value == "{" {
			depth++
			if depth > 128 {
				return nil, invalid("Caddy 配置嵌套超过限制")
			}
		}
	}
	if depth != 0 {
		return nil, invalid("Caddy 配置块未闭合")
	}
	return out, nil
}
func validateCaddyRewrite(source string) error {
	tokens, err := caddyTokens(source)
	if err != nil {
		return err
	}
	for _, token := range tokens {
		value := strings.Trim(token.value, "\"`")
		if slices.Contains([]string{"import", "tls", "bind", "root", "reverse_proxy"}, value) || strings.Contains(value, "{$") {
			return invalid("Caddy 伪静态不能导入配置或修改监听、根目录、代理及证书")
		}
		if token.depth != 0 || token.value == "}" {
			continue
		}
		begin := strings.LastIndexByte(source[:token.start], '\n') + 1
		if strings.TrimSpace(source[begin:token.start]) != "" {
			continue
		}
		if !slices.Contains([]string{"try_files", "rewrite", "redir", "respond", "uri", "handle", "handle_path", "route"}, value) && !strings.HasPrefix(value, "@") {
			return invalid("Caddy 伪静态仅允许匹配器、路径重写、跳转和响应规则")
		}
	}
	return nil
}
