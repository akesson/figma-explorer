# AiUsageDailyRow

## Properties

Name | Type | Description | Notes
------------ | ------------- | ------------- | -------------
**plan_id** | **String** | The id of the plan the usage belongs to. | 
**user_id** | **String** | The id of the Figma user that consumed the credits. | 
**user_email** | Option<**String**> | The email of the Figma user that consumed the credits, or `null` when the user's email could not be resolved (e.g. a deleted user). | [optional]
**day** | **String** | The calendar date (UTC) of the aggregated usage, in `YYYY-MM-DD` format. | 
**editor_type** | **String** | The editor the AI action was associated with. `not_applicable` when the underlying AI action had no associated file. | 
**seat_credits_sum** | **i32** | The sum of seat-level (per-user-allocated) credits consumed for this day, user, and editor type. | 
**plan_credits_sum** | **i32** | The sum of plan-level (shared pool) credits consumed for this day, user, and editor type. | 
**workspace_id** | Option<**String**> | The id of the workspace the usage was attributed to, or `null` when the usage had no associated workspace. | [optional]
**workspace_name** | Option<**String**> | The name of the workspace the usage was attributed to, or `null` when the usage had no associated workspace. | [optional]
**team_id** | Option<**String**> | The id of the team the usage was attributed to, or `null` when the usage had no associated team. | [optional]
**team_name** | Option<**String**> | The name of the team the usage was attributed to, or `null` when the usage had no associated team. | [optional]
**license_group_id** | Option<**String**> | The id of the license group the usage was attributed to, or `null` when the usage had no associated license group. | [optional]
**license_group_name** | Option<**String**> | The name of the license group the usage was attributed to, or `null` when the usage had no associated license group. | [optional]
**metering_period_start** | **String** | The start of the plan-scoped metering period this usage belongs to, as an RFC 3339 UTC timestamp (e.g. `2026-05-01T00:00:00Z`). | 
**metering_period_end** | **String** | The end of the plan-scoped metering period this usage belongs to, as an RFC 3339 UTC timestamp (e.g. `2026-06-01T00:00:00Z`). | 

[[Back to Model list]](../README.md#documentation-for-models) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to README]](../README.md)


