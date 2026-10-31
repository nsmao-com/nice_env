package core

import (
	"context"
	"encoding/json"
	"sort"
	"strings"
)

func trayGroup(id string) int {
	base, _, _ := strings.Cut(id, "@")
	switch base {
	case "nginx", "apache", "caddy":
		return 0
	case "php", "node", "python", "go":
		return 1
	case "mysql", "postgresql", "mongodb":
		return 2
	case "redis", "memcached", "rabbitmq":
		return 3
	}
	if strings.Contains(base, "jdk") || strings.Contains(base, "jre") || strings.HasPrefix(base, "java") || strings.HasPrefix(base, "temurin") {
		return 1
	}
	return 4
}

// Snapshot reads cached process state. Opening the tray never scans OS processes.
func (c *Core) registerTray() {
	c.handle("tray_panel_state", func(ctx context.Context, _ json.RawMessage) (any, error) {
		statuses := c.serviceStatuses()
		sort.Slice(statuses, func(i, j int) bool {
			a, b := trayGroup(statuses[i].ID), trayGroup(statuses[j].ID)
			if a != b {
				return a < b
			}
			return statuses[i].ID < statuses[j].ID
		})
		services, stacks, sites := []map[string]any{}, []map[string]any{}, []map[string]any{}
		readErrors := []string{}
		running, active := 0, 0
		for _, s := range statuses {
			if s.State == "running" {
				running++
			}
			if len(s.PIDs) > 0 || s.State == "starting" || s.State == "stopping" {
				active++
			}
			services = append(services, map[string]any{"id": s.ID, "label": s.Label, "state": s.State, "pids": s.PIDs, "version": s.Version, "port": s.Port, "group": trayGroup(s.ID)})
		}
		stackData, err := c.handlers["list_stacks"](ctx, nil)
		if err != nil {
			readErrors = append(readErrors, "读取服务栈："+err.Error())
		} else {
			for _, s := range stackData.([]Stack) {
				if len(stacks) == 4 {
					break
				}
				count := 0
				for _, item := range s.Items {
					for _, status := range statuses {
						if status.State == "running" && (status.ID == item.ServiceID || status.ID+"@"+status.Version == item.ServiceID || strings.HasPrefix(status.ID, item.ServiceID+"@")) {
							count++
							break
						}
					}
				}
				stacks = append(stacks, map[string]any{"id": s.ID, "name": s.Name, "running": count, "total": len(s.Items)})
			}
		}
		favorites := []string{}
		if text, _, err := c.Store.setting(ctx, "favoriteSites"); err != nil {
			readErrors = append(readErrors, err.Error())
		} else if text != "" {
			if err = json.Unmarshal([]byte(text), &favorites); err != nil {
				readErrors = append(readErrors, "收藏站点设置无效")
			}
		}
		favorite := map[string]bool{}
		for _, id := range favorites {
			favorite[id] = true
		}
		rows, err := c.Store.db.QueryContext(ctx, siteSelect+" ORDER BY updated_at DESC")
		if err != nil {
			readErrors = append(readErrors, "读取站点："+err.Error())
		} else {
			selected := []Site{}
			for rows.Next() {
				s, e := scanSite(rows)
				if e != nil {
					readErrors = append(readErrors, e.Error())
					break
				}
				selected = append(selected, s)
			}
			if err = rows.Err(); err != nil {
				readErrors = append(readErrors, err.Error())
			}
			rows.Close()
			sort.SliceStable(selected, func(i, j int) bool { return favorite[selected[i].ID] && !favorite[selected[j].ID] })
			for _, s := range selected {
				if len(sites) == 8 {
					break
				}
				sites = append(sites, map[string]any{"id": s.ID, "name": s.Name, "https": s.HTTPS, "favorite": favorite[s.ID]})
			}
		}
		appearance, _, err := c.Store.setting(ctx, "appearance")
		if err != nil {
			readErrors = append(readErrors, err.Error())
		}
		return map[string]any{"version": c.Version, "readErrors": readErrors, "appearance": appearance, "running": running, "active": active, "total": len(statuses), "dataDir": Path(c.Home), "groupTitles": []string{"Web 服务器", "语言运行时", "数据库", "缓存 / 队列", "工具 / 代理"}, "services": services, "stacks": stacks, "sites": sites}, nil
	})
}
