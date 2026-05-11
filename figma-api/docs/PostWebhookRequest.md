# PostWebhookRequest

## Properties

Name | Type | Description | Notes
------------ | ------------- | ------------- | -------------
**event_type** | [**models::WebhookV2Event**](WebhookV2Event.md) |  | 
**team_id** | Option<**String**> | Team id to receive updates about. This is deprecated, use 'context' and 'context_id' instead. | [optional]
**context** | **String** | Context to create the webhook for. Must be \"team\", \"project\", or \"file\". | 
**context_id** | **String** | The id of the context you want to receive updates about. | 
**endpoint** | **String** | The HTTP endpoint that will receive a POST request when the event triggers. Max length 2048 characters. | 
**passcode** | **String** | String that will be passed back to your webhook endpoint to verify that it is being called by Figma. Max length 100 characters. | 
**status** | Option<[**models::WebhookV2Status**](WebhookV2Status.md)> | State of the webhook, including any error state it may be in | [optional]
**description** | Option<**String**> | User provided description or name for the webhook. Max length 150 characters. | [optional]

[[Back to Model list]](../README.md#documentation-for-models) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to README]](../README.md)


