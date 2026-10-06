use clap::Subcommand;
use std::path::PathBuf;

#[derive(Debug, clap::Args)]
pub(crate) struct FacesArgs {
    /// Face index file [default: faces.sqlite in the anytopdf data directory,
    /// or ANYTOPDF_FACE_INDEX].
    #[arg(long, global = true, value_name = "PATH")]
    pub(crate) face_index: Option<PathBuf>,
    #[command(subcommand)]
    pub(crate) command: FacesCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum FacesCommand {
    /// Add a person from photos of their face (the largest face in each photo).
    Enroll {
        /// The person's name.
        name: String,
        /// Photos showing the person.
        #[arg(required = true)]
        images: Vec<PathBuf>,
    },
    /// Enroll everyone in a folder with one subfolder of photos per person.
    Import {
        /// Folder such as `faces/` holding `faces/<Name>/*.jpg`.
        dir: PathBuf,
    },
    /// List enrolled people and unnamed `person-N` clusters.
    List {
        /// Emit one JSON document on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Give a person a different name.
    Rename {
        /// Current name or `person-N` label.
        person: String,
        /// New name.
        new_name: String,
    },
    /// Name an unnamed `person-N`; merges into an existing person of that name.
    Name {
        /// The `person-N` label (or current name).
        person: String,
        /// The name to give them.
        name: String,
    },
    /// Combine two people who are the same person.
    Merge {
        /// Name or label of the person to merge away.
        from: String,
        /// Name or label of the person to keep.
        into: String,
    },
    /// Delete a person with all their stored faces and sightings.
    Forget {
        /// Name or `person-N` label.
        person: String,
    },
    /// Find every file and time where the face in a photo appears.
    Find {
        /// Photos of the face to look for.
        #[arg(required = true)]
        photos: Vec<PathBuf>,
        /// Cosine similarity at or above which a face counts as a match.
        #[arg(long, default_value_t = anytopdf_faces::recognize::DEFAULT_THRESHOLD)]
        threshold: f32,
        /// Emit one JSON document on stdout.
        #[arg(long)]
        json: bool,
    },
}
