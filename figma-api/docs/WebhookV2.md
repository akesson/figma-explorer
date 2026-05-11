# WebhookV2

## Properties

Name | Type | Description | Notes
------------ | ------------- | ------------- | -------------
**id** | **String** | The ID of the webhook | 
**event_type** | [**models::WebhookV2Event**](WebhookV2Event.md) | The event this webhook triggers on | 
**team_id** | **String** | The team id you are subscribed to for updates. This is deprecated, use context and context_id instead | 
**context** | **String** | The type of context this webhook is attached to. The value will be \"PROJECT\", \"TEAM\", or \"FILE\" | 
**context_id** | **String** | The ID of the context this webhook is attached to | 
**plan_api_id** | **String** | The plan API ID of the team or organization where this webhook was created | 
**status** | [**models::WebhookV2Status**](WebhookV2Status.md) | The current status of the webhook | 
**client_id** | Option<**String**> | The client ID of the OAuth application that registered this webhook, if any | 
**passcode** | **String** | The passcode that will be passed back to the webhook endpoint. For security, when using the GET endpoints, the value is an empty string | 
**endpoint** | **String** | The endpoint that will be hit when the webhook is triggered | 
**description** | Option<**String**> | Optional user-provided description or name for the webhook. This is provided to help make maintaining a number of webhooks more convenient. Max length 140 characters. | 

[[Back to Model list]](../README.md#documentation-for-models) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to README]](../README.md)


