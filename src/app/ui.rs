use crate::app::state::*;
use crate::format_duration;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, Paragraph};
use ratatui::Frame;
use ratatui_image::StatefulImage;

struct GradientBackground {
    dominant: (u8, u8, u8),
}

impl ratatui::widgets::Widget for GradientBackground {
    fn render(self, area: ratatui::layout::Rect, buf: &mut ratatui::buffer::Buffer) {
        for y in area.top()..area.bottom() {
            let factor = 1.0 - ((y - area.top()) as f32 / area.height as f32);
            let r = (self.dominant.0 as f32 * factor) as u8;
            let g = (self.dominant.1 as f32 * factor) as u8;
            let b = (self.dominant.2 as f32 * factor) as u8;

            for x in area.left()..area.right() {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_bg(ratatui::style::Color::Rgb(r, g, b));
                }
            }
        }
    }
}

pub fn ui(f: &mut Frame, state: &mut AppState) {
    if state.fullscreen_player {
        if let Some((r, g, b)) = state.dominant_color {
            f.render_widget(
                GradientBackground {
                    dominant: (r, g, b),
                },
                f.area(),
            );
        }
    }

    let is_vim_cmd = match state.current_view {
        View::Tracks {
            is_searching,
            ref search_query,
            ..
        } => is_searching || !search_query.is_empty(),
        View::SearchGlobal {
            is_typing,
            ref query,
            ..
        } => is_typing || !query.is_empty(),
        _ => false,
    };

    let (top, mid, cmd, bot) = if state.fullscreen_player {
        (0_u16, 0_u16, 0_u16, f.area().height.saturating_sub(4))
    } else {
        (
            3_u16,
            1_u16,
            if is_vim_cmd { 1 } else { 0 },
            if state.player_state.is_some() { 8 } else { 3 },
        )
    };

    let constraints = if state.fullscreen_player {
        vec![
            Constraint::Length(0),
            Constraint::Length(0),
            Constraint::Min(1),
            Constraint::Length(0),
        ]
    } else {
        vec![
            Constraint::Length(top),
            Constraint::Min(mid),
            Constraint::Length(bot),
            Constraint::Length(cmd),
        ]
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints(constraints)
        .split(f.area());

    let mut render_view = !state.fullscreen_player;
    let mut actual_view_chunk = chunks[1];
    let mut actual_cmd_chunk = if chunks.len() > 3 {
        chunks[3]
    } else {
        chunks[1]
    };

    if state.fullscreen_player && state.show_popup {
        render_view = true;
        let popup_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(10),
                Constraint::Percentage(80),
                Constraint::Percentage(10),
            ])
            .split(f.area());

        let popup_area = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(15),
                Constraint::Percentage(70),
                Constraint::Percentage(15),
            ])
            .split(popup_layout[1])[1];

        let inner_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(if is_vim_cmd { 1 } else { 0 }),
            ])
            .split(popup_area);

        actual_view_chunk = inner_chunks[0];
        actual_cmd_chunk = inner_chunks[1];

        // Strip natively via clear
        f.render_widget(ratatui::widgets::Clear, popup_area);
        // Paint a light translucent background natively mapped via Empty Block
        f.render_widget(
            Block::default().style(Style::default().bg(ratatui::style::Color::Black)),
            popup_area,
        );
    }

    if !state.fullscreen_player {
        // Top banner
        let nav_hint = match state.current_view {
            View::Playlists => "(↑/↓ Nav, +/- Vol, s Search, o Others, b Featured, e Queue, r Refresh, Enter View, i Mode, q Quit)",
            View::Tracks { is_searching, .. } => {
                if is_searching { "(Type to search, Enter/Esc to exit search)" }
                else { "(↑/↓ Nav, +/- Vol, / Search, A Album, a Add, e Queue, Esc Edit, Enter PLAY, i Mode, q Quit)" }
            }
            View::SearchGlobal { is_typing, .. } => {
                if is_typing { "(Type to search... Enter to search, Esc Back)" }
                else { "(↑/↓ Nav, Enter PLAY, a Add Playlist, s Search, Esc Back)" }
            }
            View::SelectPlaylist { .. } => "(↑/↓ Nav, Enter Select, Esc Back)",
            View::LoadingTracks { .. } => "(Loading...)",
        };

        let welcome_msg = format!(
            "SpotMe Client - Welcome, {}! {}",
            state.display_name, nav_hint
        );
        let banner = Paragraph::new(welcome_msg)
            .block(Block::default().borders(Borders::ALL).title("User Info"))
            .style(Style::default().fg(Color::Cyan));
        f.render_widget(banner, chunks[0]);
    }

    if render_view {
        // Active View
        match &mut state.current_view {
            View::Playlists => {
                let items: Vec<ListItem> = state
                    .filtered_playlists
                    .iter()
                    .map(|p| ListItem::new(p.name.clone()))
                    .collect();

                let playlist_list = List::new(items)
                    .block(
                        Block::default()
                            .title("Your Playlists")
                            .borders(Borders::ALL),
                    )
                    .style(Style::default().fg(Color::White))
                    .highlight_style(
                        Style::default()
                            .bg(Color::Green)
                            .fg(Color::Black)
                            .add_modifier(Modifier::BOLD),
                    )
                    .highlight_symbol(">> ");

                f.render_stateful_widget(
                    playlist_list,
                    actual_view_chunk,
                    &mut state.playlist_state,
                );
            }
            View::LoadingTracks { spinner_tick } => {
                let spinner = vec!["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                let idx = (*spinner_tick as usize) % spinner.len();

                let p = Paragraph::new(format!("{} Loading tracks...", spinner[idx]))
                    .block(Block::default().borders(Borders::ALL).title("Loading"))
                    .style(Style::default().fg(Color::Yellow));

                f.render_widget(p, actual_view_chunk);
            }
            View::Tracks {
                playlist_id: _,
                playlist_name,
                tracks,
                state: list_state,
                search_query,
                is_searching,
            } => {
                let items: Vec<ListItem> = tracks
                    .iter()
                    .map(|t| {
                        let metadata = format!(
                            "{} | {} ({})",
                            t.artist,
                            t.album,
                            format_duration(t.duration_ms)
                        );
                        let line1 = Line::from(Span::styled(
                            t.name.clone(),
                            Style::default().add_modifier(Modifier::BOLD),
                        ));
                        let line2 = Line::from(Span::styled(
                            metadata,
                            Style::default().fg(Color::DarkGray),
                        ));
                        ListItem::new(vec![line1, line2])
                    })
                    .collect();

                let title = format!("Tracks in {}", playlist_name);

                let tracks_list = List::new(items)
                    .block(Block::default().title(title).borders(Borders::ALL))
                    .style(Style::default().fg(Color::White))
                    .highlight_style(Style::default().bg(Color::Magenta).fg(Color::Black))
                    .highlight_symbol(">> ");

                f.render_stateful_widget(tracks_list, actual_view_chunk, list_state);

                // Render vim command bar
                if *is_searching || !search_query.is_empty() {
                    let cursor = if *is_searching { "█" } else { "" };
                    let cmd_text = format!("/{}{}", search_query, cursor);
                    let p = Paragraph::new(cmd_text).style(Style::default().fg(Color::Yellow));
                    f.render_widget(p, actual_cmd_chunk);
                }
            }
            View::SearchGlobal {
                query,
                tracks,
                state: list_state,
                is_typing,
            } => {
                let title = if *is_typing {
                    "Global Search (Typing...)"
                } else {
                    "Global Search"
                };
                let display_text = if let Some(t) = tracks {
                    if t.is_empty() {
                        vec![ListItem::new("No results found.")]
                    } else {
                        t.iter()
                            .map(|tr| {
                                let metadata = format!(
                                    "{} | {} ({})",
                                    tr.artist,
                                    tr.album,
                                    format_duration(tr.duration_ms)
                                );
                                let line1 = Line::from(Span::styled(
                                    tr.name.clone(),
                                    Style::default().add_modifier(Modifier::BOLD),
                                ));
                                let line2 = Line::from(Span::styled(
                                    metadata,
                                    Style::default().fg(Color::DarkGray),
                                ));
                                ListItem::new(vec![line1, line2])
                            })
                            .collect()
                    }
                } else {
                    vec![ListItem::new("Enter a query to search Spotify network...")]
                };

                let tracks_list = List::new(display_text)
                    .block(Block::default().title(title).borders(Borders::ALL))
                    .style(Style::default().fg(Color::White))
                    .highlight_style(
                        Style::default()
                            .bg(Color::Blue)
                            .fg(Color::Black)
                            .add_modifier(Modifier::BOLD),
                    )
                    .highlight_symbol(">> ");

                f.render_stateful_widget(tracks_list, actual_view_chunk, list_state);

                if *is_typing || !query.is_empty() {
                    let cursor = if *is_typing { "█" } else { "" };
                    let cmd_text = format!("Search: {}{}", query, cursor);
                    let p = Paragraph::new(cmd_text).style(Style::default().fg(Color::Yellow));
                    f.render_widget(p, actual_cmd_chunk);
                }
            }
            View::SelectPlaylist {
                track_name,
                state: list_state,
                ..
            } => {
                let items: Vec<ListItem> = state
                    .filtered_playlists
                    .iter()
                    .map(|p| ListItem::new(p.name.clone()))
                    .collect();

                let title = format!("Add '{}' to Playlist", track_name);
                let playlist_list = List::new(items)
                    .block(Block::default().title(title).borders(Borders::ALL))
                    .style(Style::default().fg(Color::White))
                    .highlight_style(
                        Style::default()
                            .bg(Color::Red)
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    )
                    .highlight_symbol(">> ");

                f.render_stateful_widget(playlist_list, actual_view_chunk, list_state);
            }
        }
    }

    // Bottom Player Box
    let player_block = Block::default().borders(Borders::ALL);
    let pdx = 2; // Fixed index now that cmd is at the end
    let inner_area = player_block.inner(chunks[pdx]);
    f.render_widget(player_block, chunks[pdx]);

    if let Some(player) = &state.player_state {
        let has_lyrics = if let Some(lyrics) = &player.lyrics {
            lyrics.synced.is_some() || lyrics.plain.is_some()
        } else {
            false
        };

        let (player_area, lyrics_area) = if state.fullscreen_player && has_lyrics {
            let v_split = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(inner_area);
            (v_split[0], Some(v_split[1]))
        } else {
            (inner_area, None)
        };

        let h_split_constraints = if state.fullscreen_player {
            vec![Constraint::Percentage(50), Constraint::Percentage(50)]
        } else {
            vec![Constraint::Length(16), Constraint::Min(0)]
        };

        let split = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(h_split_constraints)
            .split(player_area);
        let sub_chunks = split.to_vec();

        if let Some(protocol) = state.current_art_protocol.as_mut() {
            let img_widget = StatefulImage::default();
            f.render_stateful_widget(img_widget, sub_chunks[0], protocol);
        } else {
            let placeholder = Paragraph::new("\n\n ░░░░░░\n NO ART\n ░░░░░░")
                .style(
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                )
                .alignment(ratatui::layout::Alignment::Center);
            f.render_widget(placeholder, sub_chunks[0]);
        }

        let target_area = sub_chunks[1];

        let v_split_constraints = if state.fullscreen_player {
            vec![
                Constraint::Percentage(35),
                Constraint::Length(1), // Track Name
                Constraint::Length(1), // Artist
                Constraint::Length(2), // Fixed padding
                Constraint::Length(1), // Gauge
                Constraint::Length(1), // Status
                Constraint::Percentage(35),
            ]
        } else {
            vec![
                Constraint::Min(1),    // Top pad
                Constraint::Length(1), // Track Name
                Constraint::Length(1), // Artist
                Constraint::Length(1), // Fixed padding
                Constraint::Length(1), // Gauge
                Constraint::Length(1), // Status
            ]
        };

        let detail_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(v_split_constraints)
            .split(target_area);

        let align = if state.fullscreen_player {
            ratatui::layout::Alignment::Center
        } else {
            ratatui::layout::Alignment::Left
        };

        let track_name = Paragraph::new(Line::from(vec![Span::styled(
            player.track_name.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )]))
        .alignment(align);

        let artist_name = Paragraph::new(Line::from(vec![Span::styled(
            player.artist.to_uppercase(),
            Style::default().fg(Color::DarkGray),
        )]))
        .alignment(align);

        let status = if player.is_buffering {
            let spinners = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let idx = (state.player_spinner_tick as usize) % spinners.len();
            spinners[idx].to_string()
        } else if player.is_playing {
            "⏵".to_string()
        } else {
            "⏸".to_string()
        };

        let total_vol_blocks = 8;
        let filled_vol = ((player.volume_percent as u32 * total_vol_blocks) / 100) as usize;
        let filled_vol = filled_vol.min(total_vol_blocks as usize);
        let empty_vol = (total_vol_blocks as usize).saturating_sub(filled_vol);
        let vol_bar = format!("{}{}", "▰".repeat(filled_vol), "▱".repeat(empty_vol));

        let status_split = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(0), Constraint::Length(20)])
            .split(detail_chunks[5]);

        let mut status_str = format!(
            "{}   {} / {}",
            status,
            format_duration(player.progress_ms),
            format_duration(player.duration_ms)
        );

        if state.fullscreen_player && !has_lyrics {
            if player.lyrics.is_none() {
                let spinners = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                let idx = (state.player_spinner_tick as usize) % spinners.len();
                status_str.push_str(&format!("  |  Fetching lyrics... {}", spinners[idx]));
            } else {
                status_str.push_str("  |  No lyrics found");
            }
        }

        let left_status = Paragraph::new(status_str).style(Style::default().fg(Color::Gray));

        let right_status = Paragraph::new(format!("VOL {} {:3}%", vol_bar, player.volume_percent))
            .style(Style::default().fg(Color::Gray))
            .alignment(ratatui::layout::Alignment::Right);

        let mut progress_ratio = 0.0;
        if player.duration_ms > 0 {
            progress_ratio =
                (player.progress_ms as f64 / player.duration_ms as f64).clamp(0.0, 1.0);
        }

        let gauge = Gauge::default()
            .gauge_style(Style::default().fg(Color::Green).bg(Color::DarkGray))
            .ratio(progress_ratio);

        f.render_widget(track_name, detail_chunks[1]);
        f.render_widget(artist_name, detail_chunks[2]);
        f.render_widget(gauge, detail_chunks[4]);
        f.render_widget(left_status, status_split[0]);
        f.render_widget(right_status, status_split[1]);

        if state.fullscreen_player && has_lyrics {
            let lyrics_chunk = lyrics_area.unwrap();
            let lyrics_block = Block::default()
                .borders(Borders::NONE)
                .padding(ratatui::widgets::Padding::new(0, 0, 1, 0));

            let inner_lyrics_area = lyrics_block.inner(lyrics_chunk);
            f.render_widget(lyrics_block, lyrics_chunk);

            if let Some(lyrics) = &player.lyrics {
                if let Some(synced) = &lyrics.synced {
                    let mut active_idx = 0;
                    for (i, line) in synced.iter().enumerate() {
                        if player.progress_ms >= line.time_ms {
                            active_idx = i;
                        } else {
                            break;
                        }
                    }

                    let mut lyric_spans = Vec::new();

                    if state.lyrics_mode == LyricsMode::Focused {
                        let pad_top = inner_lyrics_area.height.saturating_sub(5) / 2;
                        for _ in 0..pad_top {
                            lyric_spans.push(Line::from(vec![Span::raw(" ")]));
                        }

                        let prev_idx2 = active_idx.saturating_sub(2);
                        if prev_idx2 < active_idx && active_idx >= 2 {
                            let text = &synced[prev_idx2].text;
                            lyric_spans.push(Line::from(vec![Span::styled(
                                if text.is_empty() { " " } else { text }.to_string(),
                                Style::default().fg(Color::DarkGray),
                            )]));
                        }

                        let prev_idx = active_idx.saturating_sub(1);
                        if prev_idx < active_idx && active_idx >= 1 {
                            let text = &synced[prev_idx].text;
                            lyric_spans.push(Line::from(vec![Span::styled(
                                if text.is_empty() { " " } else { text }.to_string(),
                                Style::default().fg(Color::Gray),
                            )]));
                        }

                        let text = &synced[active_idx].text;
                        lyric_spans.push(Line::from(vec![Span::styled(
                            if text.is_empty() { " " } else { text }.to_string(),
                            Style::default()
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD),
                        )]));

                        let next_idx = active_idx.saturating_add(1);
                        if next_idx < synced.len() {
                            let text = &synced[next_idx].text;
                            lyric_spans.push(Line::from(vec![Span::styled(
                                if text.is_empty() { " " } else { text }.to_string(),
                                Style::default().fg(Color::Gray),
                            )]));
                        }

                        let next_idx2 = active_idx.saturating_add(2);
                        if next_idx2 < synced.len() {
                            let text = &synced[next_idx2].text;
                            lyric_spans.push(Line::from(vec![Span::styled(
                                if text.is_empty() { " " } else { text }.to_string(),
                                Style::default().fg(Color::DarkGray),
                            )]));
                        }
                    } else {
                        let visible_lines = inner_lyrics_area.height as usize;
                        let max_scroll = synced.len().saturating_sub(visible_lines);
                        let start_idx = state.lyrics_scroll_offset.min(max_scroll);

                        for (i, line) in synced
                            .iter()
                            .enumerate()
                            .skip(start_idx)
                            .take(visible_lines)
                        {
                            let text = &line.text;
                            if text.is_empty() {
                                lyric_spans.push(Line::from(vec![Span::raw(" ")]));
                                continue;
                            }
                            if i == active_idx {
                                lyric_spans.push(Line::from(vec![Span::styled(
                                    text.clone(),
                                    Style::default()
                                        .fg(Color::LightCyan)
                                        .add_modifier(Modifier::BOLD)
                                        .add_modifier(Modifier::UNDERLINED),
                                )]));
                            } else {
                                lyric_spans.push(Line::from(vec![Span::styled(
                                    text.clone(),
                                    Style::default().fg(Color::Gray),
                                )]));
                            }
                        }
                    }

                    let p =
                        Paragraph::new(lyric_spans).alignment(ratatui::layout::Alignment::Center);
                    f.render_widget(p, inner_lyrics_area);
                } else if let Some(plain) = &lyrics.plain {
                    let text_lines: Vec<&str> = plain.lines().collect();
                    let visible_lines = inner_lyrics_area.height as usize;
                    let max_scroll = text_lines.len().saturating_sub(visible_lines);
                    let start_idx = state.lyrics_scroll_offset.min(max_scroll);

                    let mut lyric_spans = Vec::new();
                    for line in text_lines.iter().skip(start_idx).take(visible_lines) {
                        lyric_spans.push(Line::from(vec![Span::styled(
                            line.to_string(),
                            Style::default().fg(Color::Gray),
                        )]));
                    }

                    let p = Paragraph::new(lyric_spans)
                        .alignment(ratatui::layout::Alignment::Center)
                        .wrap(ratatui::widgets::Wrap { trim: false });
                    f.render_widget(p, inner_lyrics_area);
                } else {
                    let p = Paragraph::new("Lyrics not physically found on LRCLIB.")
                        .style(Style::default().fg(Color::DarkGray))
                        .alignment(ratatui::layout::Alignment::Center);
                    f.render_widget(p, inner_lyrics_area);
                }
            } else {
                let spinners = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                let idx = (state.player_spinner_tick as usize) % spinners.len();
                let txt = format!("Fetching lyrics... {}", spinners[idx]);
                let p = Paragraph::new(txt)
                    .style(Style::default().fg(Color::DarkGray))
                    .alignment(ratatui::layout::Alignment::Center);
                f.render_widget(p, inner_lyrics_area);
            }
        }
    } else {
        let text = Paragraph::new(
            "\n  No track currently playing. Select a track and press Enter to begin playback.",
        )
        .style(Style::default().fg(Color::DarkGray));
        f.render_widget(text, inner_area);
    }

    if state.show_help {
        let help_text = vec![
            Line::from(vec![Span::styled(
                " SpotMe Shortcuts ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )]),
            Line::from(""),
            Line::from(vec![Span::styled(
                "Global / Playback",
                Style::default().add_modifier(Modifier::UNDERLINED),
            )]),
            Line::from("  Space      : Play/Pause"),
            Line::from("  n / p      : Next / Previous Track"),
            Line::from("  ← / →      : Seek -5s / +5s"),
            Line::from("  h / l      : Seek -15s / +15s (Or toggle Fullscreen Lyrics via 'l')"),
            Line::from("  + / -      : Volume Up / Down"),
            Line::from("  ?          : Toggle this Help Menu"),
            Line::from("  i          : Cycle Image Renderer Protocol"),
            Line::from(""),
            Line::from(vec![Span::styled(
                "Navigation",
                Style::default().add_modifier(Modifier::UNDERLINED),
            )]),
            Line::from("  ↑/↓ & j/k  : Navigate Lists / Scroll Fullscreen Lyrics"),
            Line::from("  Enter      : Select / Play"),
            Line::from("  Esc        : Go Back / Cancel Prompts"),
            Line::from("  s          : Search Global"),
            Line::from("  o          : Switch to Playlist Views"),
            Line::from("  f          : Fullscreen Player Toggle"),
            Line::from(""),
            Line::from(vec![Span::styled(
                "Press Esc or ? to close.",
                Style::default().fg(Color::DarkGray),
            )]),
        ];

        let popup_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(15),
                Constraint::Percentage(70),
                Constraint::Percentage(15),
            ])
            .split(f.area());

        let popup_area = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(25),
                Constraint::Percentage(50),
                Constraint::Percentage(25),
            ])
            .split(popup_layout[1])[1];

        // Explicitly clear background behind the popup so it doesn't mesh with the UI!
        f.render_widget(ratatui::widgets::Clear, popup_area);

        let p = Paragraph::new(help_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Yellow))
                    .style(Style::default().bg(Color::Reset)),
            )
            .alignment(ratatui::layout::Alignment::Left);

        f.render_widget(p, popup_area);
    }
}
