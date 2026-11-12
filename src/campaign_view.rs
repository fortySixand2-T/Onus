//! The campaign client's **view** (C6): it draws what
//! [`crate::campaign_client`] holds and changes none of it. A text panel
//! shows the current screen's text; on the map, gizmos draw the roads, the
//! cities in their holder's colour (a capital ringed twice), the armies,
//! and what the keys point at; each city carries its name. In a battle the
//! skirmish's own input and presentation run on the battle's field, the
//! camera drawn back to show both bases.
//!
//! Render types live here and only here; the headless tests run the
//! client's logic without this module. How it looks is the user's to judge
//! on the Mac (F-099).

use bevy::prelude::*;

use crate::campaign::Location;
use crate::campaign_client::{
    battle_text, diplomacy_text, map_text, over_text, story_text, CampaignSession, LiveBattle, Screen,
};
use crate::client::{nation_color, ClickTracker, CursorWorld, DragState};
use crate::{input, ui};

/// Where the map is drawn: layout units are scaled and shifted right, so
/// the text panel on the left does not cover it.
const MAP_SCALE: f32 = 0.8;
const MAP_OFFSET: Vec2 = Vec2::new(200.0, 0.0);
const CITY_RADIUS: f32 = 12.0;
const ARMY_SIZE: f32 = 9.0;
const ROAD_COLOR: Color = Color::srgb(0.45, 0.45, 0.45);
const CURSOR_COLOR: Color = Color::srgb(1.0, 0.9, 0.2);
const RAISE_COLOR: Color = Color::srgb(0.95, 0.95, 0.95);
/// The camera's zoom on a battle: the bases are 1500 apart.
const BATTLE_SCALE: f32 = 1.4;

/// The text panel.
#[derive(Component)]
pub struct CampaignPanel;

/// A city's name label (the campaign city index).
#[derive(Component)]
pub struct CityLabel(pub usize);

/// Register the view on `app` (the shipped campaign app only).
pub fn add_campaign_view(app: &mut App) {
    app.init_resource::<CursorWorld>()
        .init_resource::<DragState>()
        .init_resource::<ClickTracker>()
        .add_systems(Startup, spawn_view)
        .add_systems(
            Update,
            (
                update_panel,
                show_labels,
                frame_camera,
                draw_map.run_if(in_state(Screen::Map).or_else(in_state(Screen::Diplomacy))),
            ),
        )
        .add_systems(
            Update,
            (
                input::update_cursor,
                input::selection.after(input::update_cursor),
                input::emit_commands
                    .after(input::update_cursor)
                    .run_if(crate::sim::victory::match_running),
                input::emit_build_commands
                    .after(input::update_cursor)
                    .run_if(crate::sim::victory::match_running),
                ui::attach_visuals,
                ui::sync_transform,
                ui::draw_selection,
                ui::draw_nation_marks,
                ui::show_wells,
                ui::draw_well_rings,
            )
                .run_if(in_state(Screen::Battle))
                .run_if(resource_exists::<LiveBattle>),
        );
}

/// The map at its own scale; a battle drawn back to show both bases.
fn frame_camera(screen: Res<State<Screen>>, mut cameras: Query<&mut Projection, With<Camera2d>>) {
    let want = if *screen.get() == Screen::Battle { BATTLE_SCALE } else { 1.0 };
    for mut p in &mut cameras {
        if let Projection::Orthographic(o) = p.as_mut() {
            if o.scale != want {
                o.scale = want;
            }
        }
    }
}

fn on_screen(layout_pos: Vec2) -> Vec2 {
    layout_pos * MAP_SCALE + MAP_OFFSET
}

fn spawn_view(mut commands: Commands, session: Res<CampaignSession>) {
    commands.spawn(Camera2d);
    commands.spawn((
        Text::new(""),
        TextFont {
            font_size: bevy::text::FontSize::Px(12.0),
            ..default()
        },
        TextColor(Color::srgb(0.9, 0.9, 0.9)),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(10.0),
            top: Val::Px(10.0),
            max_width: Val::Px(430.0),
            ..default()
        },
        CampaignPanel,
    ));
    for (i, p) in session.layout.positions().iter().enumerate() {
        let at = on_screen(*p) + Vec2::new(0.0, -CITY_RADIUS - 10.0);
        commands.spawn((
            Text2d::new(session.campaign.campaign().cities[i].name.clone()),
            TextFont {
                font_size: bevy::text::FontSize::Px(11.0),
                ..default()
            },
            TextColor(Color::srgb(0.85, 0.85, 0.85)),
            Transform::from_xyz(at.x, at.y, 1.0),
            CityLabel(i),
        ));
    }
}

fn update_panel(
    session: Res<CampaignSession>,
    live: Option<Res<LiveBattle>>,
    screen: Res<State<Screen>>,
    mut panel: Query<&mut Text, With<CampaignPanel>>,
) {
    let text = match screen.get() {
        Screen::Story => story_text(&session),
        Screen::Map => map_text(&session),
        Screen::Diplomacy => diplomacy_text(&session),
        Screen::Battle => battle_text(&session, live.as_deref()),
        Screen::Over => over_text(&session),
    };
    for mut t in &mut panel {
        if t.0 != text {
            t.0 = text.clone();
        }
    }
}

fn show_labels(screen: Res<State<Screen>>, mut labels: Query<&mut Visibility, With<CityLabel>>) {
    let shown = matches!(screen.get(), Screen::Map | Screen::Diplomacy);
    let want = if shown { Visibility::Inherited } else { Visibility::Hidden };
    for mut v in &mut labels {
        if *v != want {
            *v = want;
        }
    }
}

fn draw_map(mut gizmos: Gizmos, session: Res<CampaignSession>) {
    let c = &session.campaign;
    let pos = |i: usize| on_screen(session.layout.positions()[i]);
    for r in &c.campaign().roads {
        gizmos.line_2d(pos(r.a), pos(r.b), ROAD_COLOR);
    }
    for (i, city) in c.cities().iter().enumerate() {
        let colour = nation_color(c.content(), Some(city.owner));
        gizmos.circle_2d(Isometry2d::from_translation(pos(i)), CITY_RADIUS, colour);
        if c.campaign().capital[city.owner] == i {
            gizmos.circle_2d(Isometry2d::from_translation(pos(i)), CITY_RADIUS + 4.0, colour);
        }
    }
    // Armies: a square at their city (side by side when several stand
    // there), or partway along the road they march.
    let mut stacked = vec![0u32; c.cities().len()];
    for a in c.armies() {
        let at = match a.location {
            Location::At(city) => {
                let k = stacked[city];
                stacked[city] += 1;
                pos(city) + Vec2::new(CITY_RADIUS + 8.0 + k as f32 * (ARMY_SIZE + 3.0), 8.0)
            }
            Location::Marching { road, from, to, progress } => {
                let cost = c.campaign().roads[road].cost.max(1) as f32;
                pos(from).lerp(pos(to), (progress as f32 / cost).clamp(0.0, 1.0))
            }
        };
        let colour = nation_color(c.content(), Some(a.nation));
        gizmos.rect_2d(Isometry2d::from_translation(at), Vec2::splat(ARMY_SIZE), colour);
        if session.cursor.army == Some(a.id) {
            gizmos.rect_2d(Isometry2d::from_translation(at), Vec2::splat(ARMY_SIZE + 6.0), CURSOR_COLOR);
        }
    }
    if let Some(t) = session.target_city() {
        gizmos.circle_2d(Isometry2d::from_translation(pos(t)), CITY_RADIUS + 9.0, CURSOR_COLOR);
    }
    if let Some(k) = session.cursor.city {
        gizmos.circle_2d(Isometry2d::from_translation(pos(k)), CITY_RADIUS + 13.0, RAISE_COLOR);
    }
}
