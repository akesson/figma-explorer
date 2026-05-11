# WebhookFileCommentPayload

## Properties

Name | Type | Description | Notes
------------ | ------------- | ------------- | -------------
**passcode** | **String** | The passcode specified when the webhook was created, should match what was initially provided | 
**timestamp** | **String** | UTC ISO 8601 timestamp of when the event was triggered. | 
**webhook_id** | **String** | The id of the webhook that caused the callback | 
**event_type** | **String** |  | 
**comment** | [**Vec<models::CommentFragment>**](CommentFragment.md) | Contents of the comment itself | 
**comment_id** | **String** | Unique identifier for comment | 
**created_at** | **String** | The UTC ISO 8601 time at which the comment was left | 
**file_key** | **String** | The key of the file that was commented on | 
**file_name** | **String** | The name of the file that was commented on | 
**mentions** | Option<[**Vec<models::User>**](User.md)> | Users that were mentioned in the comment | [optional]
**triggered_by** | [**models::User**](User.md) | The user that made the comment and triggered this event | 

[[Back to Model list]](../README.md#documentation-for-models) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to README]](../README.md)


