#![allow(clippy::all)]

use bevy::prelude::*;
use bevy_trait_query::*;
use criterion::*;
use std::fmt::Display;

/// Define a trait for our components to implement.
#[queryable]
pub trait Messages {
    fn messages(&self) -> &[String];
    fn send_message(&mut self, _: &dyn Display);
}

#[derive(Component)]
pub struct RecA {
    messages: Vec<String>,
}

impl Messages for RecA {
    fn messages(&self) -> &[String] {
        &self.messages
    }
    fn send_message(&mut self, msg: &dyn Display) {
        self.messages.push(msg.to_string());
    }
}

#[derive(Component)]
pub struct RecB {
    messages: Vec<String>,
}

impl Messages for RecB {
    fn messages(&self) -> &[String] {
        &self.messages
    }
    fn send_message(&mut self, msg: &dyn Display) {
        self.messages.push(msg.to_string());
    }
}

pub struct Benchmark<'w>(World, QueryState<One<&'w dyn Messages>>, Vec<usize>);

impl<'w> Benchmark<'w> {
    // Each entity only has one component in practice.
    fn one() -> Self {
        let mut world = World::new();

        world.register_component_as::<dyn Messages, RecA>();
        world.register_component_as::<dyn Messages, RecB>();

        for _ in 0..5_000 {
            world.spawn((Name::new("Hello"), RecA { messages: vec![] }));
        }
        for _ in 0..5_000 {
            world.spawn((Name::new("Hello"), RecB { messages: vec![] }));
        }

        let query = world.query();
        Self(world, query, default())
    }
    // There will be some entities that have multiple trait impls, and will be filtered out.
    pub fn filtered() -> Self {
        let mut world = World::new();

        world.register_component_as::<dyn Messages, RecA>();
        world.register_component_as::<dyn Messages, RecB>();

        for _ in 0..2_500 {
            world.spawn((Name::new("Hello"), RecA { messages: vec![] }));
        }
        for _ in 0..2_500 {
            world.spawn((Name::new("Hello"), RecB { messages: vec![] }));
        }
        for _ in 0..5_000 {
            world.spawn((
                Name::new("Hello"),
                RecA { messages: vec![] },
                RecB { messages: vec![] },
            ));
        }

        let query = world.query();
        Self(world, query, default())
    }

    pub fn run(&mut self) {
        let mut output = Vec::new();
        for x in self.1.iter_mut(&mut self.0) {
            output.push(x.messages().len());
        }
        self.2 = output;
    }
}

pub fn one_match(c: &mut Criterion) {
    let mut benchmark = Benchmark::one();
    c.bench_function("One<>", |b| b.iter(|| benchmark.run()));
    eprintln!("{}", benchmark.2.len());
}
pub fn filtering(c: &mut Criterion) {
    let mut benchmark = Benchmark::filtered();
    c.bench_function("One<> - filtering", |b| b.iter(|| benchmark.run()));
    eprintln!("{}", benchmark.2.len());
}

criterion_group!(one, one_match, filtering);
criterion_main!(one);
