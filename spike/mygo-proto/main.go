package main

import (
	"fmt"
	"log"
	"os"
	"time"

	"github.com/egoist/mygo"
	"github.com/egoist/mygo/ui"
)

var (
	cBG     = ui.Hex("#0B0C0E")
	cBar    = ui.Hex("#0D0E11")
	cText   = ui.Hex("#F2F3F5")
	cMuted  = ui.Hex("#8A8F98")
	cAccent = ui.Hex("#4C8DFF")
	cOK     = ui.Hex("#3DDC97")
	cWarn   = ui.Hex("#FFB020")
	cErr    = ui.Hex("#FF5C5C")
	cTrack  = ui.Hex("#1A1D22")
	cSel    = ui.Hex("#1C2026")
)

type kindT int

const (
	kindHTTP kindT = iota
	kindTorrent
)

type statusT int

const (
	stActive statusT = iota
	stPaused
	stDone
	stFailed
)

type row struct {
	name    string
	size    uint64
	done    uint64
	speed   uint64
	kind    kindT
	status  statusT
	links   int
	country string
}

func (r row) frac() float32 {
	if r.size == 0 {
		return 0
	}
	return float32(float64(r.done) / float64(r.size))
}

func fmtBytes(n uint64) string {
	f := float64(n)
	if f < 1024 {
		return fmt.Sprintf("%d B", n)
	}
	for _, u := range []string{"KB", "MB", "GB", "TB"} {
		f /= 1024
		if f < 1024 {
			return fmt.Sprintf("%.1f %s", f, u)
		}
	}
	return fmt.Sprintf("%.1f PB", f/1024)
}

func fmtSpeed(n uint64) string {
	if n == 0 {
		return ""
	}
	return fmtBytes(n) + "/s"
}

var names = []string{
	"ubuntu-24.04.1-desktop-amd64.iso",
	"debian-13.0.0-netinst-amd64.iso",
	"holiday-photos-2026.zip",
	"project-report-q4-final-v3.pdf",
	"rust-1.99.1-x86_64-pc-windows-gnu.msi",
	"open-data-2026.csv",
	"backup-2026-11.tar.zst",
	"invoice-batch-0921.xlsx",
	"podcast-episode-42.mp3",
}

var countries = []string{"JP", "SG", "US", "DE", "NL", "KR"}

func sampleRows(n int) []row {
	seed := uint64(0x2545F4914F6CDD1D)
	next := func() uint64 {
		seed = seed*6364136223846793005 + 1442695040888963407
		return seed >> 11
	}
	out := make([]row, 0, n)
	for i := 0; i < n; i++ {
		r := row{name: names[i%len(names)]}
		r.size = 12*(1<<20) + next()%(900<<20)
		r.speed = (1 << 20) + next()%(28<<20)
		r.links = int(1 + next()%8)
		r.country = countries[next()%uint64(len(countries))]
		if i%9 == 0 {
			r.kind = kindTorrent
			r.name = names[(i+3)%len(names)] + ".torrent"
		}
		switch roll := next() % 100; {
		case roll < 15:
			r.status = stDone
			r.done = r.size
			r.speed = 0
		case roll < 80:
			r.status = stActive
			r.done = r.size * (5 + next()%80) / 100
		case roll < 90:
			r.status = stPaused
			r.done = r.size * (5 + next()%80) / 100
			r.speed = 0
		default:
			r.status = stFailed
			r.done = r.size * (5 + next()%40) / 100
			r.speed = 0
		}
		out = append(out, r)
	}
	return out
}

type app struct {
	rows   []row
	list   ui.ListState
	sel    int
	first  time.Time
	themed bool
}

func statusLabel(r row) string {
	switch r.status {
	case stDone:
		return "Done"
	case stPaused:
		return "Paused"
	case stFailed:
		return "Failed"
	}
	return fmtSpeed(r.speed)
}

func statusColor(r row) ui.Color {
	switch r.status {
	case stDone:
		return cOK
	case stPaused:
		return cMuted
	case stFailed:
		return cErr
	}
	return cAccent
}

func (a *app) view(c *ui.Context) {
	if a.first.IsZero() {
		a.first = time.Now()
	}
	a.applyTheme(c)

	ui.Column(c).Fill().Background(cBG).Children(func() {
		ui.Row(c).FillWidth().Height(56).Padding(0, 20).AlignItems(ui.Center).Gap(14).Children(func() {
			ui.Text(c, "Volant").FontSize(15).Bold().TextColor(cText).Grow(1)
			ui.Text(c, "+").FontSize(17).TextColor(cMuted)
			ui.Text(c, "*").FontSize(15).TextColor(cAccent)
		})

		ui.List(c, &a.list, len(a.rows)).Grow(1).Rows(func(row ui.ListRow) {
			i := row.Index
			r := a.rows[i]
			bg := ui.Transparent
			col := statusColor(r)
			ctx := row.Context
			body := func() {
				if i == a.sel {
					ui.Box(ctx).Absolute().Left(0).Top(0).Width(2).Height(52).Background(cAccent)
				}
				ui.Column(ctx).FillWidth().Height(52).Justify(ui.Center).Children(func() {
					ui.Row(ctx).FillWidth().Height(46).Padding(0, 20).AlignItems(ui.Center).Children(func() {
						ui.Text(ctx, r.name).FontSize(14.5).TextColor(cText).Grow(1)
						ui.Text(ctx, statusLabel(r)).FontSize(13).TextColor(col).Width(96)
						ui.Text(ctx, fmtBytes(r.size)).FontSize(13).TextColor(cMuted).Width(84)
					})
					ui.Row(ctx).FillWidth().Height(2).Padding(0, 20).Children(func() {
						ui.Row(ctx).FillWidth().Height(2).Background(cTrack).Children(func() {
							ui.Row(ctx).WidthPercent(r.frac() * 100).Height(2).Background(col)
						})
					})
				})
			}
			el := ui.Box(ctx).FillWidth().Height(52).Background(bg)
			if el.Hovered() {
				el.Background(ui.Hex("#16181C"))
			}
			if i == a.sel {
				el.Background(cSel)
			}
			if el.Clicked() {
				a.sel = i
			}
			el.Children(body)
		})

		ui.Column(c).FillWidth().Height(88).Padding(0, 20).Justify(ui.Center).Gap(5).Children(func() {
			var sel row
			if a.sel >= 0 && a.sel < len(a.rows) {
				sel = a.rows[a.sel]
			}
			ui.Row(c).FillWidth().Height(28).AlignItems(ui.Center).Children(func() {
				ui.Text(c, sel.name).FontSize(14.5).TextColor(cText).Grow(1)
				ui.Text(c, statusLabel(sel)).FontSize(13).TextColor(cAccent)
			})
			meta := fmt.Sprintf("%s · %d conns · %s", fmtBytes(sel.done), sel.links, sel.country)
			if sel.speed > 0 {
				meta += " · ETA 0:04"
			}
			ui.Text(c, meta).FontSize(13).TextColor(cMuted)
		})
	})
}

func (a *app) applyTheme(c *ui.Context) {
	if a.themed {
		return
	}
	a.themed = true
	t := ui.DarkTheme()
	t.Background = cBG
	t.Surface = cBar
	t.Text = cText
	t.TextMuted = cMuted
	t.Accent = cAccent
	t.Selection = cSel
	t.Border = cTrack
	c.SetTheme(t)
}

var started = time.Now()

func main() {
	a := &app{rows: sampleRows(1000), sel: 1}
	bench := os.Getenv("VOLANT_BENCH") != ""
	shotPath := os.Getenv("VOLANT_SHOT")

	mygo.App.WhenReady(func() {
		win := mygo.NewWindow(mygo.WindowOptions{
			Title:   "Volant",
			Width:   780,
			Height:  470,
			Content: ui.View(a.view),
		})
		if bench {
			go func() {
				time.Sleep(4 * time.Second)
				fmt.Printf("RESULT ui=mygo startup_ms=%.1f rows=%d\n",
					float64(a.first.Sub(started).Microseconds())/1000.0, len(a.rows))
				os.Exit(0)
			}()
		}
		if shotPath != "" {
			go func() {
				time.Sleep(2 * time.Second)
				png, err := win.CapturePage()
				if err != nil {
					fmt.Println("capture error:", err)
					os.Exit(1)
				}
				if err := os.WriteFile(shotPath, png, 0o644); err != nil {
					fmt.Println("write error:", err)
					os.Exit(1)
				}
				fmt.Println("shot saved", shotPath)
				os.Exit(0)
			}()
		}
	})

	if err := mygo.App.Run(); err != nil {
		log.Fatal(err)
	}
}
