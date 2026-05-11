use clap::Subcommand;
use figma_api::apis::configuration::Configuration;

pub mod activity_logs;
pub mod comment_reactions;
pub mod comments;
pub mod component_sets;
pub mod components;
pub mod dev_resources;
pub mod files;
pub mod library_analytics;
pub mod o_embed;
pub mod payments;
pub mod projects;
pub mod styles;
pub mod users;
pub mod variables;
pub mod webhooks;

#[derive(Subcommand, Debug)]
pub enum Command {
    /// GET /v1/me — authenticated user profile.
    Me,
    /// GET /v1/activity_logs — workspace activity log events.
    ActivityLogs(activity_logs::ActivityLogsArgs),
    /// GET /v1/files/{key}/comments/{id}/reactions — comment reactions.
    CommentReactions(comment_reactions::CommentReactionsArgs),
    /// GET /v1/files/{key}/comments — file comments.
    Comments(comments::CommentsArgs),
    /// GET /v1/component_sets/{key} — published component set metadata.
    ComponentSet(component_sets::ComponentSetArgs),
    /// GET /v1/files/{key}/component_sets — file component sets.
    FileComponentSets(component_sets::FileComponentSetsArgs),
    /// GET /v1/teams/{team_id}/component_sets — team component sets.
    TeamComponentSets(component_sets::TeamComponentSetsArgs),
    /// GET /v1/components/{key} — published component metadata.
    Component(components::ComponentArgs),
    /// GET /v1/files/{key}/components — file components.
    FileComponents(components::FileComponentsArgs),
    /// GET /v1/teams/{team_id}/components — team components.
    TeamComponents(components::TeamComponentsArgs),
    /// GET /v1/files/{key}/dev_resources — dev resources.
    DevResources(dev_resources::DevResourcesArgs),
    /// GET /v1/files/{key} — file document.
    File(files::FileArgs),
    /// GET /v1/files/{key}/meta — file metadata.
    FileMeta(files::FileMetaArgs),
    /// GET /v1/files/{key}/nodes — specific nodes from a file.
    FileNodes(files::FileNodesArgs),
    /// GET /v1/files/{key}/versions — file version history.
    FileVersions(files::FileVersionsArgs),
    /// GET /v1/files/{key}/images — image-fill download URLs.
    ImageFills(files::ImageFillsArgs),
    /// GET /v1/images/{key} — render nodes as images.
    Images(files::ImagesArgs),
    /// GET /v1/analytics/libraries/{key}/component/actions — library analytics: component actions.
    LibraryAnalyticsComponentActions(library_analytics::ComponentActionsArgs),
    /// GET /v1/analytics/libraries/{key}/component/usages — library analytics: component usages.
    LibraryAnalyticsComponentUsages(library_analytics::ComponentUsagesArgs),
    /// GET /v1/analytics/libraries/{key}/style/actions — library analytics: style actions.
    LibraryAnalyticsStyleActions(library_analytics::StyleActionsArgs),
    /// GET /v1/analytics/libraries/{key}/style/usages — library analytics: style usages.
    LibraryAnalyticsStyleUsages(library_analytics::StyleUsagesArgs),
    /// GET /v1/analytics/libraries/{key}/variable/actions — library analytics: variable actions.
    LibraryAnalyticsVariableActions(library_analytics::VariableActionsArgs),
    /// GET /v1/analytics/libraries/{key}/variable/usages — library analytics: variable usages.
    LibraryAnalyticsVariableUsages(library_analytics::VariableUsagesArgs),
    /// GET /v1/oembed — oEmbed data for a Figma URL.
    OEmbed(o_embed::OEmbedArgs),
    /// GET /v1/payments — user payment info for a plugin, widget, or community file.
    Payments(payments::PaymentsArgs),
    /// GET /v1/projects/{project_id}/files — files in a project.
    ProjectFiles(projects::ProjectFilesArgs),
    /// GET /v1/teams/{team_id}/projects — projects in a team.
    TeamProjects(projects::TeamProjectsArgs),
    /// GET /v1/files/{key}/styles — file styles.
    FileStyles(styles::FileStylesArgs),
    /// GET /v1/styles/{key} — published style metadata.
    Style(styles::StyleArgs),
    /// GET /v1/teams/{team_id}/styles — team styles.
    TeamStyles(styles::TeamStylesArgs),
    /// GET /v1/files/{key}/variables/local — local variables in a file.
    LocalVariables(variables::LocalVariablesArgs),
    /// GET /v1/files/{key}/variables/published — published variables in a file.
    PublishedVariables(variables::PublishedVariablesArgs),
    /// GET /v2/teams/{team_id}/webhooks — team webhooks.
    TeamWebhooks(webhooks::TeamWebhooksArgs),
    /// GET /v2/webhooks/{id} — single webhook.
    Webhook(webhooks::WebhookArgs),
    /// GET /v2/webhooks/{id}/requests — recent webhook delivery attempts.
    WebhookRequests(webhooks::WebhookRequestsArgs),
    /// GET /v2/webhooks — webhooks for a context or plan.
    Webhooks(webhooks::WebhooksArgs),
}

impl Command {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        match self {
            Self::Me => users::run_me(cfg).await,
            Self::ActivityLogs(a) => a.run(cfg).await,
            Self::CommentReactions(a) => a.run(cfg).await,
            Self::Comments(a) => a.run(cfg).await,
            Self::ComponentSet(a) => a.run(cfg).await,
            Self::FileComponentSets(a) => a.run(cfg).await,
            Self::TeamComponentSets(a) => a.run(cfg).await,
            Self::Component(a) => a.run(cfg).await,
            Self::FileComponents(a) => a.run(cfg).await,
            Self::TeamComponents(a) => a.run(cfg).await,
            Self::DevResources(a) => a.run(cfg).await,
            Self::File(a) => a.run(cfg).await,
            Self::FileMeta(a) => a.run(cfg).await,
            Self::FileNodes(a) => a.run(cfg).await,
            Self::FileVersions(a) => a.run(cfg).await,
            Self::ImageFills(a) => a.run(cfg).await,
            Self::Images(a) => a.run(cfg).await,
            Self::LibraryAnalyticsComponentActions(a) => a.run(cfg).await,
            Self::LibraryAnalyticsComponentUsages(a) => a.run(cfg).await,
            Self::LibraryAnalyticsStyleActions(a) => a.run(cfg).await,
            Self::LibraryAnalyticsStyleUsages(a) => a.run(cfg).await,
            Self::LibraryAnalyticsVariableActions(a) => a.run(cfg).await,
            Self::LibraryAnalyticsVariableUsages(a) => a.run(cfg).await,
            Self::OEmbed(a) => a.run(cfg).await,
            Self::Payments(a) => a.run(cfg).await,
            Self::ProjectFiles(a) => a.run(cfg).await,
            Self::TeamProjects(a) => a.run(cfg).await,
            Self::FileStyles(a) => a.run(cfg).await,
            Self::Style(a) => a.run(cfg).await,
            Self::TeamStyles(a) => a.run(cfg).await,
            Self::LocalVariables(a) => a.run(cfg).await,
            Self::PublishedVariables(a) => a.run(cfg).await,
            Self::TeamWebhooks(a) => a.run(cfg).await,
            Self::Webhook(a) => a.run(cfg).await,
            Self::WebhookRequests(a) => a.run(cfg).await,
            Self::Webhooks(a) => a.run(cfg).await,
        }
    }
}
