# \WebhooksApi

All URIs are relative to *https://api.figma.com*

Method | HTTP request | Description
------------- | ------------- | -------------
[**delete_webhook**](WebhooksApi.md#delete_webhook) | **DELETE** /v2/webhooks/{webhook_id} | Delete a webhook
[**get_team_webhooks**](WebhooksApi.md#get_team_webhooks) | **GET** /v2/teams/{team_id}/webhooks | [Deprecated] Get team webhooks
[**get_webhook**](WebhooksApi.md#get_webhook) | **GET** /v2/webhooks/{webhook_id} | Get a webhook
[**get_webhook_requests**](WebhooksApi.md#get_webhook_requests) | **GET** /v2/webhooks/{webhook_id}/requests | Get webhook requests
[**get_webhooks**](WebhooksApi.md#get_webhooks) | **GET** /v2/webhooks | Get webhooks by context or plan
[**post_webhook**](WebhooksApi.md#post_webhook) | **POST** /v2/webhooks | Create a webhook
[**put_webhook**](WebhooksApi.md#put_webhook) | **PUT** /v2/webhooks/{webhook_id} | Update a webhook



## delete_webhook

> models::WebhookV2 delete_webhook(webhook_id)
Delete a webhook

Deletes the specified webhook. This operation cannot be reversed.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**webhook_id** | **String** | ID of webhook to delete | [required] |

### Return type

[**models::WebhookV2**](WebhookV2.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_team_webhooks

> models::InlineObject27 get_team_webhooks(team_id)
[Deprecated] Get team webhooks

Returns all webhooks registered under the specified team.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**team_id** | **String** | ID of team to get webhooks for | [required] |

### Return type

[**models::InlineObject27**](inline_object_27.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_webhook

> models::WebhookV2 get_webhook(webhook_id)
Get a webhook

Get a webhook by ID.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**webhook_id** | **String** | ID of webhook to get | [required] |

### Return type

[**models::WebhookV2**](WebhookV2.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_webhook_requests

> models::InlineObject28 get_webhook_requests(webhook_id)
Get webhook requests

Returns all webhook requests sent within the last week. Useful for debugging.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**webhook_id** | **String** | The id of the webhook subscription you want to see events from | [required] |

### Return type

[**models::InlineObject28**](inline_object_28.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_webhooks

> models::InlineObject26 get_webhooks(context, context_id, plan_api_id, cursor)
Get webhooks by context or plan

Returns a list of webhooks corresponding to the context or plan provided, if they exist. For plan, the webhooks for all contexts that you have access to will be returned, and theresponse is paginated

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**context** | Option<**String**> | Context to create the resource on. Should be \"team\", \"project\", or \"file\". |  |
**context_id** | Option<**String**> | The id of the context that you want to get attached webhooks for. If you're using context_id, you cannot use plan_api_id. |  |
**plan_api_id** | Option<**String**> | The id of your plan. Use this to get all webhooks for all contexts you have access to. If you're using plan_api_id, you cannot use context or context_id. When you use plan_api_id, the response is paginated. |  |
**cursor** | Option<**String**> | If you're using plan_api_id, this is the cursor to use for pagination. If you're using context or context_id, this parameter is ignored. Provide the next_page or prev_page value from the previous response to get the next or previous page of results. |  |

### Return type

[**models::InlineObject26**](inline_object_26.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## post_webhook

> models::WebhookV2 post_webhook(post_webhook_request)
Create a webhook

Create a new webhook which will call the specified endpoint when the event triggers. By default, this webhook will automatically send a PING event to the endpoint when it is created. If this behavior is not desired, you can create the webhook and set the status to PAUSED and reactivate it later.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**post_webhook_request** | [**PostWebhookRequest**](PostWebhookRequest.md) | The webhook to create. | [required] |

### Return type

[**models::WebhookV2**](WebhookV2.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: application/json
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## put_webhook

> models::WebhookV2 put_webhook(webhook_id, put_webhook_request)
Update a webhook

Update a webhook by ID.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**webhook_id** | **String** | ID of webhook to update | [required] |
**put_webhook_request** | [**PutWebhookRequest**](PutWebhookRequest.md) | The webhook to update. | [required] |

### Return type

[**models::WebhookV2**](WebhookV2.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: application/json
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)

