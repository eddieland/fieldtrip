//! The shortest ways to reach a type from `Query`, one for each field of `Query` that leads to it.
//!
//! The schema is a graph of types. A field is a step from its type to the type it returns, and an interface or union
//! leads to each of its possible types through `... on Type`, which is free: it selects nothing. A search outward from
//! `Query` would have to be repeated for every root field, so the distances are found once, backwards from the target,
//! and each path is then read off by following them down.

use std::collections::VecDeque;
use std::fmt::Write as _;

use apollo_compiler::Name;
use apollo_compiler::collections::HashMap;
use apollo_compiler::schema::ExtendedType;

use super::Loaded;

/// How many paths `describe` shows before counting the rest.
const MAX_PATHS: usize = 5;

/// One way out of a type.
enum Step<'schema> {
    /// A field, which costs one step.
    Field(&'schema str, &'schema str),
    /// `... on Type` from an interface or a union, which is free.
    Cast(&'schema str),
}

impl<'schema> Step<'schema> {
    /// The type this step arrives at.
    const fn target(&self) -> &'schema str {
        match self {
            Self::Field(_, target) | Self::Cast(target) => target,
        }
    }

    /// How many fields this step adds to a path.
    const fn length(&self) -> usize {
        match self {
            Self::Field(..) => 1,
            Self::Cast(_) => 0,
        }
    }
}

/// Writes the shortest paths from `Query` to the type called `name`, as `# paths from Query:` and one line each, or
/// `# no path from Query`. Input objects, root types and built-in types get nothing, since a path is something to
/// select and there is nothing to say about reaching `Query` or `String`.
pub(super) fn paths(out: &mut String, loaded: &Loaded, name: &str) {
    let Some(ty) = loaded.schema.types.get(name) else {
        return;
    };
    if ty.is_built_in() || loaded.is_root(name) || matches!(ty, ExtendedType::InputObject(_)) {
        return;
    }
    let Some(query) = loaded.schema.schema_definition.query.as_ref() else {
        return;
    };
    let Some(ExtendedType::Object(query)) = loaded.schema.types.get(query.name.as_str()) else {
        return;
    };

    let graph = Graph::new(loaded);
    let distances = graph.distances_to(name);
    let mut found: Vec<(usize, String)> = query
        .fields
        .values()
        .filter_map(|field| {
            let start = field.ty.inner_named_type().as_str();
            let distance = distances.get(start)?;
            let mut path = format!("{}.{}", query.name, field.name);
            graph.follow(&mut path, start, name, &distances);
            Some((1 + distance, path))
        })
        .collect();
    // A stable sort keeps the order of the fields of `Query` among paths of one length.
    found.sort_by_key(|(length, _)| *length);

    if found.is_empty() {
        let _ = writeln!(out, "# no path from {}", query.name);
        return;
    }
    let _ = writeln!(out, "# paths from {}:", query.name);
    for (_, path) in found.iter().take(MAX_PATHS) {
        let _ = writeln!(out, "#   {path}");
    }
    if found.len() > MAX_PATHS {
        let _ = writeln!(
            out,
            "#   and {} more, from other fields of {}",
            found.len() - MAX_PATHS,
            query.name
        );
    }
}

/// The ways out of each type that can be selected, leaving out the root types: a path that went back through `Query`
/// would only be a longer way to start.
struct Graph<'schema> {
    /// Each type's steps: its fields in their order, then its possible types.
    steps: HashMap<&'schema str, Vec<Step<'schema>>>,
}

impl<'schema> Graph<'schema> {
    /// Builds the graph of `loaded`.
    fn new(loaded: &'schema Loaded) -> Self {
        let roots = loaded.root_names();
        let selectable = |name: &str| {
            !roots.contains(&name)
                && loaded
                    .schema
                    .types
                    .get(name)
                    .is_some_and(|ty| !ty.is_built_in() && !matches!(ty, ExtendedType::InputObject(_)))
        };
        let mut steps: HashMap<&str, Vec<Step<'_>>> = HashMap::default();
        for (name, ty) in &loaded.schema.types {
            let name = name.as_str();
            if !selectable(name) {
                continue;
            }
            let fields = match ty {
                ExtendedType::Object(object) => Some(&object.fields),
                ExtendedType::Interface(interface) => Some(&interface.fields),
                _ => None,
            };
            let mut out: Vec<Step<'_>> = fields
                .into_iter()
                .flat_map(|fields| fields.values())
                .map(|field| Step::Field(field.name.as_str(), field.ty.inner_named_type().as_str()))
                .filter(|step| selectable(step.target()))
                .collect();
            match ty {
                ExtendedType::Union(union) => {
                    out.extend(union.members.iter().map(|member| Step::Cast(member.name.as_str())));
                }
                ExtendedType::Interface(_) => {
                    if let Some(implementers) = loaded.implementers.get(name) {
                        // By name, since the schema's order of types depends on where the schema came from.
                        let mut possible: Vec<&str> = implementers
                            .objects
                            .iter()
                            .chain(&implementers.interfaces)
                            .map(Name::as_str)
                            .collect();
                        possible.sort_unstable();
                        out.extend(possible.into_iter().map(Step::Cast));
                    }
                }
                _ => {}
            }
            steps.insert(name, out);
        }
        Self { steps }
    }

    /// How many fields it takes to reach `target` from each type that can reach it.
    fn distances_to(&self, target: &'schema str) -> HashMap<&'schema str, usize> {
        let mut back: HashMap<&str, Vec<(&str, usize)>> = HashMap::default();
        for (from, steps) in &self.steps {
            for step in steps {
                back.entry(step.target()).or_default().push((from, step.length()));
            }
        }
        // Breadth first, with free steps at the front of the queue so that they are taken before longer ones.
        let mut distances: HashMap<&str, usize> = HashMap::default();
        distances.insert(target, 0);
        let mut queue = VecDeque::from([target]);
        while let Some(to) = queue.pop_front() {
            let distance = distances.get(to).copied().unwrap_or_default();
            for &(from, length) in back.get(to).into_iter().flatten() {
                let through = distance + length;
                if distances.get(from).is_some_and(|known| *known <= through) {
                    continue;
                }
                distances.insert(from, through);
                if length == 0 {
                    queue.push_front(from);
                } else {
                    queue.push_back(from);
                }
            }
        }
        distances
    }

    /// Appends to `path` the shortest way from `start` to `target`: a field where one leads closer, or `... on Type`
    /// where a possible type is as close.
    fn follow(&self, path: &mut String, start: &str, target: &str, distances: &HashMap<&str, usize>) {
        let mut at = start;
        // Every step lowers the distance or casts to a narrower type, so this many steps is always enough.
        for _ in 0..=self.steps.len() {
            if at == target {
                return;
            }
            let Some(&distance) = distances.get(at) else {
                return;
            };
            let steps = self.steps.get(at).map_or(&[][..], Vec::as_slice);
            let next = steps
                .iter()
                .find(|step| matches!(step, Step::Field(..)) && closer(step, distance, distances))
                .or_else(|| steps.iter().find(|step| closer(step, distance, distances)));
            match next {
                Some(Step::Field(field, to)) => {
                    let _ = write!(path, ".{field}");
                    at = to;
                }
                Some(Step::Cast(to)) => {
                    let _ = write!(path, "{{... on {to}}}");
                    at = to;
                }
                None => return,
            }
        }
    }
}

/// Whether `step`, taken from a type `distance` fields from the target, stays on a shortest path.
fn closer(step: &Step<'_>, distance: usize, distances: &HashMap<&str, usize>) -> bool {
    distances
        .get(step.target())
        .is_some_and(|after| after + step.length() == distance)
}
