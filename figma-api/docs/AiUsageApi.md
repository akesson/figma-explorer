# \AiUsageApi

All URIs are relative to *https://api.figma.com*

Method | HTTP request | Description
------------- | ------------- | -------------
[**get_ai_usage_daily**](AiUsageApi.md#get_ai_usage_daily) | **GET** /v1/ai_usage/daily | Get daily AI credit usage



## get_ai_usage_daily

> models::InlineObject31 get_ai_usage_daily(start_date, end_date, user_email, limit, cursor)
Get daily AI credit usage

Returns per-user, per-day AI credit usage for the plan associated with the calling token. This endpoint requires a plan access token with the `org:ai_metering_usage_read` scope.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**start_date** | **String** | The first day to include, inclusive, as a `YYYY-MM-DD` calendar date (UTC). Required. Must be on or after `2025-12-01` and no more than 366 days before the current UTC day. | [required] |
**end_date** | **String** | The last day to include, inclusive, as a `YYYY-MM-DD` calendar date (UTC). Required. Must be on or after `start_date` and the current UTC day or earlier. | [required] |
**user_email** | Option<**String**> | Restrict the results to a single Figma user, identified by email. When omitted, rows for every user in the plan with usage in the range are returned. An email that matches no Figma user returns a 400. |  |
**limit** | Option<**u16**> | Maximum number of rows to return. This param defaults to 1000 if unspecified, and may not exceed 1000. |  |[default to 1000]
**cursor** | Option<**String**> | An opaque cursor returned from a previous request, used for pagination. |  |

### Return type

[**models::InlineObject31**](inline_object_31.md)

### Authorization

[PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)

