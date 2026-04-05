use std::fmt::Display;

use anyhow::{Context, Result, bail};
use dialoguer::{Confirm, Input, MultiSelect, Password, Select, theme::ColorfulTheme};

pub fn theme() -> ColorfulTheme {
    ColorfulTheme::default()
}

pub fn prompt_select<T>(prompt: &str, items: &[T]) -> Result<usize>
where
    T: Display,
{
    if items.is_empty() {
        bail!("cannot select from an empty list");
    }

    Select::with_theme(&theme())
        .with_prompt(prompt)
        .items(items)
        .default(0)
        .interact()
        .context("failed to read selection from the terminal")
}

pub fn prompt_multi_select<T>(
    prompt: &str,
    items: &[T],
    defaults: Option<&[bool]>,
) -> Result<Vec<usize>>
where
    T: Display,
{
    if items.is_empty() {
        bail!("cannot select from an empty list");
    }

    let rendered_items = items.iter().map(ToString::to_string).collect::<Vec<_>>();
    let dialog_theme = theme();
    let mut prompt_builder = MultiSelect::with_theme(&dialog_theme)
        .with_prompt(prompt)
        .items(&rendered_items)
        .report(false);
    if let Some(defaults) = defaults {
        prompt_builder = prompt_builder.defaults(defaults);
    }
    prompt_builder
        .interact()
        .context("failed to read selections from the terminal")
}

pub fn prompt_confirm(prompt: &str, default: bool) -> Result<bool> {
    Confirm::with_theme(&theme())
        .with_prompt(prompt)
        .default(default)
        .interact()
        .context("failed to read confirmation from the terminal")
}

pub fn prompt_input(prompt: &str, default: Option<&str>) -> Result<String> {
    let dialog_theme = theme();
    let mut input = Input::<String>::with_theme(&dialog_theme);
    input = input.with_prompt(prompt);
    if let Some(value) = default {
        input = input.default(value.to_owned());
    }
    input.interact_text().context("failed to read input")
}

pub fn prompt_password(prompt: &str) -> Result<String> {
    Password::with_theme(&theme())
        .with_prompt(prompt)
        .interact()
        .context("failed to read password")
}
